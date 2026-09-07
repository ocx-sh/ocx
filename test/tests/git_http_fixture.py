# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Smart-HTTP git transport for the fake forge, over real `git http-backend`.

A mixin over `FakeForge` (`fake_forge.py`), alongside `GitLabRoutes`
(`fake_gitlab.py`), serving `git clone` / `git fetch` / `git push` from the
**same host and port** as the REST surface. One host is not a convenience: C-034
scopes the pushed credential to an `http.<prefix>.extraHeader` whose prefix
carries the full project path, so "the header is not sent to a sibling project"
(S-040) is only a real question when the sibling shares the origin.

**Real `git http-backend`, shelled out to as CGI.** `dulwich` was evaluated and
rejected — its `ReceivePackHandler` never negotiates the `push-options`
capability, which is the one capability this whole transport exists to use.
`http.server.CGIHTTPRequestHandler` is deprecated since Python 3.13 and removed
in 3.15, so the bridge is hand-rolled. See
`.claude/artifacts/research_plan_index_claim_git_fixture.md`; those choices are
settled.

Four properties of the bridge are load-bearing, each because its absence fails
silently rather than loudly:

* **Every request header is forwarded as `HTTP_<NAME>`**, `Git-Protocol`
  included. A bridge passing a hard-coded env set downgrades every request to
  protocol v0, and capability-dependent behaviour then differs from a real
  server invisibly.
* **The RPC response is buffered so `Content-Length` is set.** `git
  http-backend` streams with no length; under this fixture's HTTP/1.1
  keep-alive (`fake_forge.py:74`) an unframed response hangs the client, and
  there is no `pytest-timeout` in `test/pyproject.toml` to rescue the run.
* **`http-backend`'s stderr is captured, never swallowed.** The process exits 0
  whatever `Status:` it emitted, so its exit code carries no signal at all and
  stderr is the only diagnosis.
* **`Transfer-Encoding: chunked` request bodies are decoded here** (C-074).
  `BaseHTTPRequestHandler` does not decode them anywhere; `self.rfile` is the
  raw socket stream.

**Every git route passes one authorization gate** (`_git_http_authorize`). Every
row that proves ocx resolved and injected a credential reads the *child's*
environment, and a bridge serving `git-receive-pack` to anyone makes the wire
half of that claim unfalsifiable: the push succeeds identically with no
credential at all, so nothing distinguishes "ocx authenticated" from "ocx sent
nothing and the server did not care". A missing, malformed or empty-halved
`Authorization` on the write half is answered **401** here, which is what makes
an accepted push evidence.

`git_http_credential` narrows acceptance to one exact pair, which is what gives a
*wrong* credential a state to be tested in — the bridge has no other way to know
which secret is right, because every consumer resolves its own.
`git_http_private` decides whether the **read** half demands one too, and the two
settings reach genuinely different code in ocx: a refusal on the fetch is
classified by `git_stderr.rs::credential_rejection_status`, a refusal on the push
by `classify_push_failure`. `git_http_forbid_fetch` supplies the third state —
**403**, a credential that authenticates and still may not read — which is the
other status the fetch table recovers.

**Git traffic never touches the REST log.** `git_http_get` / `git_http_post` are
dispatched from `_Handler` *before* `FakeForge.record()` runs, and a path is a
git route only when its project segment names a repository created through
`git_create_project`. A test that creates no git project therefore cannot reach
this module at all, which is what keeps `request_count` — 28 call sites in
`test_announce.py` — meaning exactly what it meant before, and keeps S-026's
"zero REST writes on every git failure" falsifiable.

**Commits are seeded git-first** (DX-5). `git init --bare` is a second, real
object store with real SHA-1s, while `fake_forge.py` mints synthetic ones. A
branch is created with real `git` in the bare repository and the resulting real
sha is then imported into the in-memory graph, so `gl_get_branch` answers the
same sha that `--force-with-lease=<branch>:<expected-sha>` (C-040) carries.
Without that reconciliation every C-042 / S-024 / S-025 assertion measures the
fixture instead of ocx.
"""

from __future__ import annotations

import base64
import binascii
import dataclasses
import enum
import http.client
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.parse
from collections.abc import Mapping, Sequence
from email.message import Message as EmailMessage
from pathlib import Path
from typing import TYPE_CHECKING, BinaryIO

if TYPE_CHECKING:  # pragma: no cover - typing only
    from fake_forge import _Handler

# ── constants ────────────────────────────────────────────────────────────────

#: Default ceiling on a decoded chunked request body. C-074 requires the decoder
#: to raise rather than truncate once a body exceeds it; `git_http_max_body_bytes`
#: lowers it per server so a test can reach the cap without sending megabytes.
MAX_CHUNKED_BODY_BYTES = 8 * 1024 * 1024

#: The branch `git init --bare -b <name>` creates. Named rather than defaulted:
#: git's own default is configurable per-host, and a repository whose default
#: branch is `master` makes every `main`-based assertion fail for a reason that
#: has nothing to do with ocx.
INITIAL_BRANCH = "main"

#: Configuration applied to every bare repository, as an explicit data table.
#:
#: * `receive.advertisePushOptions` — false by default; without it the
#:   push-option capability is never advertised and the payload never reaches a
#:   hook, so every C-039 assertion filters an empty log and passes.
#: * `http.receivepack` — anonymous receive-pack is refused by default, so
#:   without it no push is ever accepted.
#: * `uploadpack.allowFilter` — without it `--filter=blob:none` (C-036) is
#:   **silently ignored**, the fetch still succeeds, and the cost rationale the
#:   filter exists for is untested.
#: * `http.getanyfile` — the dumb protocol, on by default. Off here so a smart
#:   -HTTP defect cannot be masked by a working dumb fallback.
BARE_REPO_CONFIG: Mapping[str, str] = {
    "receive.advertisePushOptions": "true",
    "http.receivepack": "true",
    "uploadpack.allowFilter": "true",
    "http.getanyfile": "false",
}

#: The header carrying the protocol version. git sends it; the bridge must map
#: it to `HTTP_GIT_PROTOCOL`, which `http-backend` copies to `GIT_PROTOCOL`.
GIT_PROTOCOL_HEADER = "Git-Protocol"

#: Identity written into the fixture `HOME`'s `~/.gitconfig`. Deliberately NOT
#: `ocx <noreply@ocx.sh>`: C-045 asserts the commit identity ocx injects through
#: `GIT_AUTHOR_*`/`GIT_COMMITTER_*`, and against a `HOME` carrying the same
#: identity that assertion cannot tell "ocx set it" from "git read it from
#: `HOME`".
FIXTURE_HOME_USER_NAME = "Fixture Home"
FIXTURE_HOME_USER_EMAIL = "fixture-home@example.invalid"

#: Ceiling on one chunk-size or trailer line. A line that reaches it without a
#: terminator is a stream that will not end, and is refused as `TRUNCATED`
#: rather than read forever.
_MAX_LINE_BYTES = 64 * 1024

#: Default for `git_http_body_read_timeout_seconds`: how long the bridge waits
#: for the rest of a request body before abandoning the connection. There is no
#: `pytest-timeout` in `test/pyproject.toml`, so a handler blocked forever on a
#: half-sent body would leak a thread for the whole session; git over loopback
#: never pauses this long mid-body.
#:
#: Per-server rather than a constant, for the same reason
#: `git_http_max_body_bytes` is: the abandonment is only observable inside a
#: test if the test can reach it, and 30s is longer than any bound a test may
#: block for. `ThreadingHTTPServer.daemon_threads` is True and
#: `socketserver._Threads.append` returns early for daemon threads, so
#: `server_close()` never joins a blocked handler — "shutdown returned" is
#: therefore true whatever this timeout does, and the reachable observation is
#: the client's socket seeing EOF.
BODY_READ_TIMEOUT_SECONDS = 30.0

#: The body of git's `probe_rpc` — a bare pkt-line flush packet, sent as a
#: `Content-Length` request to the receive-pack URL before any chunked stream,
#: because a chunked body cannot be replayed after a 401. It carries no pack, so
#: `git_http_forbid_receive_pack` steps over it: see that knob for why.
_RECEIVE_PACK_PROBE_BODY = b"0000"

#: What `git` prints when the server refuses `GET /info/refs`, by status.
#:
#: The literals `git_stderr.rs`'s `CREDENTIAL_REJECTIONS` recovers a status from,
#: spelled here so a row can assert the CLIENT surfaced the refusal rather than
#: only that the server sent it — the two differ, and the gap between them is
#: where a challenge-less 401 hides: git reports a bare transport error for one
#: and names the credential it could not obtain for the other.
#:
#: Measured against git 2.54.0 under `git_environment` (`LC_ALL=C`,
#: `GIT_TERMINAL_PROMPT=0`). A 401 prints the same line whether the request
#: carried no credential or a rejected one: git answers both by consulting the
#: credential subsystem, and with no helper and no prompt it dies there. The 403
#: line is libcurl's and is not translated.
FETCH_REFUSAL_NEEDLES: Mapping[int, str] = {
    401: "could not read Username for ",
    403: "The requested URL returned error: 403",
}

#: The challenge every 401 here carries, because RFC 7235 requires one and
#: GitLab sends one.
#:
#: **It has no reachable red state in this suite, and the comment that claimed
#: otherwise was wrong.** Measured against git 2.54.0 by deleting this header and
#: re-running both the fixture rows and the two `--transport git` claim rows: all
#: stayed green. git converts a 401 into a credential request whether or not the
#: response carries a challenge, so `could not read Username for` — the line
#: `git_stderr.rs` matches — comes out either way. Kept for fidelity to the
#: server being modelled, not as a load-bearing guard; do not write an assertion
#: whose stated mutation is "delete the challenge", because that mutation does
#: not red.
_BASIC_CHALLENGE = 'Basic realm="ocx fake forge"'

#: How long a refused request drains the rest of its body before closing. Closing
#: a socket with unread bytes still buffered sends an RST on Linux, which
#: discards the 400 the client has not read yet — so the response would vanish
#: and the test would see a connection error instead of the status it asserts.
_DRAIN_TIMEOUT_SECONDS = 0.2

#: Ceiling on any fixture-side `git` invocation. A wedged child would otherwise
#: hang the whole suite instead of failing one test.
_GIT_TIMEOUT_SECONDS = 120.0

#: The JSONL file the receive hooks append one object to per push, and the JSON
#: file the bridge writes the current refusal/delay knobs into before every
#: receive-pack. Both live in the server's own scratch state directory and both
#: absolute paths are baked into the generated hook source.
_HOOK_LOG_NAME = "pushes.jsonl"
_HOOK_CONFIG_NAME = "hooks.json"

#: The recording `credential.helper` a `credential_helper=True` fixture `HOME`
#: installs. Python with a `sys.executable` shebang, for the same reason
#: `git_shim.py` gives: the uv venv interpreter is not on the child's `PATH`.
#: It answers nothing — the record is the point, not a credential — and no
#: credential-shaped string appears anywhere in it.
#:
#: It records **its own exit status** alongside the call, as capture and not as
#: a check — see `CredentialHelperCall.exit_status`. The status is computed and
#: written BEFORE the exit, so a record that exists is a call that reached the
#: end; a helper that died earlier writes nothing at all, and git says nothing
#: either way. What separates "no helper was invoked" from "the helper is
#: broken" is therefore the positive control
#: (`::test_credential_helper_records_an_invocation`), never this field.
#:
#: The log is opened 0600 rather than at the umask default: the stdin it
#: captures is a credential description, and `git credential fill` can put a
#: password in it. `os.open` is the only way to fix the mode at creation.
_CREDENTIAL_HELPER_SOURCE = '''#!{python}
# SPDX-License-Identifier: Apache-2.0
# Generated by `test/tests/git_http_fixture.py` — edit the renderer, not this.
import json
import os
import sys

LOG_PATH = {log}


def main():
    action = sys.argv[1] if len(sys.argv) > 1 else ""
    # The one reachable non-zero: git always passes an action, so an argv
    # without one means something other than git invoked this helper.
    status = 0 if action else 2
    payload = {{"action": action, "stdin": sys.stdin.read(), "exit": status}}
    handle = os.open(LOG_PATH, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    with os.fdopen(handle, "a", encoding="utf-8") as log:
        log.write(json.dumps(payload) + "\\n")
    sys.exit(status)


main()
'''

#: The `pre-receive` / `post-receive` hook both bare repositories carry.
#:
#: JSON rather than lines because a refusal test deliberately makes an option
#: value hostile, and line-oriented logging cannot survive a value containing a
#: newline — exactly the value C-039 must be proved to reject.
#:
#: `pre-receive` decides the refusal and writes NO record; `post-receive` writes
#: the one record per push, numbered by counting the log it is appending to
#: under an exclusive lock. That split is what makes `git_pushes()` return
#: 1, 2, 3 rather than a pair per push, and it is why a declined push leaves the
#: log untouched — the refusal is observable in the client's stderr, which is
#: where WP-12 reads it.
#:
#: The refusal line is written WITHOUT a `remote: ` prefix: `send-pack` adds
#: that itself, and a hook echoing the quoted string verbatim yields
#: `remote: remote: …`.
_HOOK_SOURCE = '''#!{python}
# SPDX-License-Identifier: Apache-2.0
# Generated by `test/tests/git_http_fixture.py` — edit the renderer, not this.
import fcntl
import json
import os
import sys
import time

CONFIG_PATH = {config}
LOG_PATH = {log}
PROJECT_ROOT = {root}
HOOK = {hook}


def project():
    """`<owner>/<repo>` for the repository this hook is running in.

    Derived from the repository's own path rather than read out of the config
    file, so two projects pushed to in one test cannot be attributed to each
    other."""
    here = os.path.realpath(os.environ.get("GIT_DIR") or ".")
    relative = os.path.relpath(here, os.path.realpath(PROJECT_ROOT))
    return relative[:-4] if relative.endswith(".git") else relative


def settings():
    try:
        with open(CONFIG_PATH, encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        return {{}}


def push_options():
    """`(present, count, values)`.

    git 2.54.0 exports `GIT_PUSH_OPTION_COUNT=0` to a receive hook whether or
    not the client negotiated `push-options`, with `receive.advertisePushOptions`
    either way, over the local transport and smart HTTP alike — measured with a
    plain `/bin/sh` hook outside this fixture (DX-19). So `present` is False on
    no git this fixture runs against, and it is capture, NOT a check: do not
    write an assertion on it, and do not "fix" this by defaulting the read —
    `environ.get(name, "0")` would report a presence the environment does not
    have, which is worse than reporting an absence nothing can reach."""
    count = os.environ.get("GIT_PUSH_OPTION_COUNT")
    if count is None:
        return False, None, []
    values = [os.environ.get("GIT_PUSH_OPTION_%d" % index, "") for index in range(int(count))]
    return True, count, values


def updates():
    parsed = []
    for line in sys.stdin.read().splitlines():
        fields = line.split()
        if len(fields) >= 3:
            parsed.append({{"old": fields[0], "new": fields[1], "ref": fields[2]}})
    return parsed


def main():
    config = settings()
    present, count, values = push_options()
    refs = updates()

    if HOOK == "pre-receive":
        refusal = config.get("refusal")
        forbidden = set(config.get("forbidden_options") or [])
        keys = set(value.split("=", 1)[0] for value in values)
        if refusal is not None and (not forbidden or keys & forbidden):
            sys.stderr.write(refusal + "\\n")
            sys.exit(1)
        sys.exit(0)

    delay = config.get("delay", 0.0)
    payload = {{
        "hook": HOOK,
        "project": project(),
        "options": values,
        "option_count_present": present,
        "option_count": count,
        "updates": refs,
        "ready_at": None if delay is None else time.time() + float(delay),
    }}
    with open(LOG_PATH, "a+", encoding="utf-8") as log:
        fcntl.flock(log.fileno(), fcntl.LOCK_EX)
        log.seek(0)
        payload["invocation"] = sum(1 for line in log if line.strip()) + 1
        log.seek(0, 2)
        log.write(json.dumps(payload) + "\\n")
        log.flush()
        fcntl.flock(log.fileno(), fcntl.LOCK_UN)
    sys.exit(0)


main()
'''


class BodyFraming(enum.StrEnum):
    """How a request framed its body, as observed on the wire.

    This is not diagnostics. A claim root's pack is far below `http.postBuffer`'s
    1 MiB default, so an ordinary push is `Content-Length`-framed and a test
    named "chunked body decoded" passes **without the decoder ever running**.
    The framing field is the only negative control that separates the two, which
    is why an assertion about chunked decoding asserts *this*, never the push's
    success.
    """

    CHUNKED = "chunked"
    CONTENT_LENGTH = "content-length"
    NONE = "none"


class ChunkedBodyRejection(enum.StrEnum):
    """Why `read_chunked_body` refused a stream (C-074).

    Distinguished rather than collapsed into one message because each has its
    own named test and each must be deletable one at a time to prove the others
    are not covering for it. `CHUNK_EXTENSION` and `TRAILER_SECTION` are
    reachable only from a hand-built raw-socket stream — real git emits
    neither.
    """

    CHUNK_EXTENSION = "chunk-extension"
    TRAILER_SECTION = "trailer-section"
    OVERSIZED = "oversized"
    MALFORMED_SIZE = "malformed-size"
    TRUNCATED = "truncated"


class BranchSeedState(enum.StrEnum):
    """The six branch states C-051 states per transport.

    `AHEAD_IDENTICAL` is a distinct commit carrying the base's tree byte for
    byte — the state where C-042 must perform no write at all when a request is
    already open, and a refresh commit when none is. Collapsing it into
    `IDENTICAL` erases the only case that distinguishes "nothing to do" from
    "nothing to commit but a request to ensure".
    """

    ABSENT = "absent"
    IDENTICAL = "identical"
    AHEAD_IDENTICAL = "ahead-identical"
    AHEAD_DIFFERING = "ahead-differing"
    BEHIND = "behind"
    DIVERGED = "diverged"


class RefusalShape(enum.StrEnum):
    """The six server-side refusals whose verbatim client stderr WP-12's phrase
    table is written against.

    Named as a closed set so the producer test cannot silently lose a row: its
    red state is "git 2.54 emits a phrase the table does not contain", and a
    table keyed by an open-ended string could never notice a missing key.

    The two 403 shapes are separate members because they are answered on
    different requests and produce *different* client-side stderr, a
    distinction C-044's phrase table does not draw.
    """

    PRE_RECEIVE_DECLINED = "pre-receive-declined"
    NOT_ALLOWED_TO_PUSH = "not-allowed-to-push"
    FORBIDDEN_INFO_REFS = "forbidden-info-refs"
    FORBIDDEN_RECEIVE_PACK = "forbidden-receive-pack"
    NON_FAST_FORWARD = "non-fast-forward"
    STALE_LEASE = "stale-lease"


#: Verbatim `git` client stderr for each refusal shape, observed against the
#: local git and published here as the INPUT to C-044's phrase table.
#:
#: WP-12 is Rust and can import neither this module nor a pytest module, so its
#: author reads these strings; one home beside `RefusalShape` is where a reader
#: looks. `""` means "not yet recorded" —
#: `::test_server_side_rejection_texts_are_recorded` drives each shape, asserts
#: every value is non-empty, and asserts the observed text equals the value
#: here. An unfilled row therefore reds rather than passing vacuously, and a
#: text git changes in a later release reds too, which is the drift this table
#: exists to catch.
#:
#: Each value is the PHRASE, not the whole stream: git's stderr embeds the
#: fixture's ephemeral port, the branch name and object counts, so a whole
#: -stream compare could never be deterministic. Recorded against **git
#: 2.54.0**, which is the version to name when one of these drifts.
#:
#: Where each phrase comes from (C-044 requires this annotation, because it
#: decides how much a matching test proves):
#:
#: * `(fetch first)` and `(stale info)` are emitted by the LOCAL git client,
#:   decided against the ref advertisement, so a fixture exercises real
#:   evidence. Both need the racing writer to land BEFORE the receive-pack
#:   advertisement; landing it before the POST instead yields `incorrect old
#:   value provided`, which is a different refusal.
#: * `(pre-receive hook declined)` is git's own wording for a hook exiting
#:   non-zero, but the line above it is whatever the FIXTURE's hook wrote.
#: * `You are not allowed to push code to this project.` proves the LEAST of
#:   the six, and is annotated because nothing else says so. It is the string
#:   the test itself writes into the hook config and then reads back out of the
#:   client's stderr — input and output share a path — so all it establishes is
#:   the round trip: that a hook line reaches the client verbatim and without a
#:   doubled `remote: ` prefix. The wording is GitLab's, quoted from the ADR,
#:   and is unproved against a real GitLab until release gate 4.
#: * The two 403 phrases are produced by the fixture answering 403, so they
#:   prove the classifier's wiring and not the phrase itself; they stay
#:   unproved against a real forge until release gate 4.
OBSERVED_REFUSAL_STDERR: Mapping[RefusalShape, str] = {
    RefusalShape.PRE_RECEIVE_DECLINED: "(pre-receive hook declined)",
    RefusalShape.NOT_ALLOWED_TO_PUSH: "You are not allowed to push code to this project.",
    RefusalShape.FORBIDDEN_INFO_REFS: "The requested URL returned error: 403",
    RefusalShape.FORBIDDEN_RECEIVE_PACK: "RPC failed; HTTP 403",
    RefusalShape.NON_FAST_FORWARD: "(fetch first)",
    RefusalShape.STALE_LEASE: "(stale info)",
}


class ChunkedBodyError(ValueError):
    """A request body the fixture refused, carrying *why* and under which framing.

    The bridge answers 400 **and closes the connection** on this. Closing is not
    politeness: the fixture speaks HTTP/1.1 with keep-alive, so a half-read body
    desynchronises the next request on that socket and the failure then surfaces
    inside an unrelated test.

    Raised from **both** framings, which is why `framing` is carried rather than
    assumed. A `Content-Length` body is no less client-controlled than a chunked
    one, so the two refuse alike: `OVERSIZED` for a declared length above the
    ceiling, `MALFORMED_SIZE` for one that is negative or not an integer at all.
    The alternative — clamping the declared length and reading a prefix — answers
    a short body as if it were complete and leaves the remainder to frame the
    next request on the socket, which is the desynchronisation the close exists
    to prevent.

    The refused request still produces a `GitRequest`, carrying that `framing`
    and this `reason` — see `GitRequest.rejection`.
    """

    def __init__(
        self,
        reason: ChunkedBodyRejection,
        framing: BodyFraming = BodyFraming.CHUNKED,
    ) -> None:
        super().__init__(f"{framing.value} request body refused: {reason.value}")
        self._reason = reason
        self._framing = framing

    @property
    def reason(self) -> ChunkedBodyRejection:
        """Which of C-074's refusals fired."""
        return self._reason

    @property
    def framing(self) -> BodyFraming:
        """The framing the refused body arrived under, so the recorded
        `GitRequest` names what was actually on the wire."""
        return self._framing


class GitFixtureError(RuntimeError):
    """A fixture-side failure: an unusable bare repository, a seeding step whose
    `git` invocation failed, an unregistered project.

    Deliberately not a `pytest.fail`: it must be catchable, because several
    tests arm a failure and assert on it.
    """


# ── captured records ─────────────────────────────────────────────────────────


@dataclasses.dataclass(slots=True, frozen=True)
class GitRequest:
    """One git-transport request, as the bridge observed it.

    `headers` is the full request header map — `HTTP_AUTHORIZATION` is the only
    server-side observation point for C-034's `http.extraHeader`, so the whole
    map is kept rather than a selected subset that a later assertion would have
    to widen.

    **Every name maps to ALL of its values, in arrival order**, and that shape
    is load-bearing rather than defensive: `http.extraHeader` is multi-valued in
    git, so a run sending both an unscoped and a scoped `Authorization` puts two
    on the wire. A `{name: value}` capture keeps whichever the dict comprehension
    saw last, and the regression — a credential that leaked alongside the
    intended one — is then unreportable. Read it through `header_values`, never
    by indexing, so a second value cannot be silently dropped at the reader
    either.

    `cgi_env` is the environment block the bridge handed `git http-backend`.
    Keeping both it and `headers` is what makes "the bridge forwards *all*
    headers" falsifiable: `headers` says what arrived, `cgi_env` says what was
    passed on, and a bridge with a hard-coded env set shows a `Git-Protocol` in
    the first and no `HTTP_GIT_PROTOCOL` in the second.

    `backend_stderr` is `http-backend`'s stderr for this request, verbatim. The
    process exits 0 whatever status it emitted, so this is the only diagnosis a
    failing request has.

    **`headers` and `cgi_env` are readable but do not appear in the repr.**
    Nearly every assertion in `test_git_http_fixture.py` interpolates a
    `GitRequest` into its failure message, so a red build prints one — and both
    fields carry the credential: `Authorization` verbatim in `headers`, the same
    value as `HTTP_AUTHORIZATION` plus the child's whole environment in
    `cgi_env`. `fake_forge.AUTH_HEADERS` already makes exactly this trade for the
    REST log; the git log differs only in that it must KEEP everything, because
    the header map is its observation point. Printing it too was never a
    decision. Read either field explicitly (`header_values`, `dict(cgi_env)`)
    when a message needs it.

    **A refused request is recorded too**, with `framing=CHUNKED`, `status=400`
    and `rejection` naming which of C-074's refusals fired; `cgi_env` is empty
    because the child never ran. Without that record the three refusal tests can
    only observe "some 400", which is the exit-code tolerance band
    `subsystem-tests.md` forbids — it cannot tell a refused chunk extension from
    a malformed request the bridge rejected for an unrelated reason.
    """

    method: str
    path: str
    query: str
    project: str
    service: str
    headers: Mapping[str, Sequence[str]] = dataclasses.field(repr=False)
    framing: BodyFraming
    body_bytes: int
    status: int
    cgi_env: Mapping[str, str] = dataclasses.field(repr=False)
    backend_stderr: str
    rejection: ChunkedBodyRejection | None = None

    def header_values(self, name: str) -> tuple[str, ...]:
        """Every value sent under `name`, in arrival order; `()` when absent.

        Case-insensitive, because header names are on the wire and the casing
        `headers` carries is whatever the client chose. The tuple is the whole
        answer: a caller that wants "the" value asserts the length it expects
        first, so a second value reds rather than disappearing.
        """
        lowered = name.lower()
        for key, values in self.headers.items():
            if key.lower() == lowered:
                return tuple(values)
        return ()


@dataclasses.dataclass(slots=True, frozen=True)
class PushedRef:
    """One `<old> <new> <ref>` triple a receive hook read from stdin.

    `old` is `0` * 40 for a ref being created — the create-shaped update that
    proves C-038's first-claim path took the create direction, and which is
    visible nowhere else on the wire.
    """

    old: str
    new: str
    ref: str


@dataclasses.dataclass(slots=True, frozen=True)
class PushRecord:
    """One receive-hook invocation, as JSON, one object per push.

    JSON rather than lines because a refusal test deliberately makes an option
    value hostile, and line-oriented logging cannot survive a value containing a
    newline — which is exactly the value C-039 must be proved to reject.

    `option_count_present` records whether `GIT_PUSH_OPTION_COUNT` was set **at
    all**, separately from `option_count`'s value. It is honest capture and
    **not a check** (DX-19): git 2.54.0 exports the variable unconditionally —
    `"0"` when the options phase was never negotiated — so `False` is a state no
    mutation of this fixture can produce, and an `is True` assertion on it can
    never go red. It stays on the record because a later git could restore the
    absent state and because a reader needs to be able to tell `"0"` from
    nothing; the discriminating assertions are on `option_count` and `options`.

    `invocation` is a 1-based counter assigned by the **hook**, not by the
    reader: it is what `::test_post_receive_hook_invocation_counter_is_nonzero`
    asserts before any option assertion, because `HOME` reaches the child by
    design (C-033) and a developer's global `core.hooksPath` — or a `noexec`
    `TMPDIR` — disables every hook, at which point every "the options parsed
    correctly" assertion filters an empty log and passes.

    It counts **pushes, not hook runs**: server-wide across the fixture's
    lifetime, one increment per push, so `git_pushes()` answers 1, 2, 3 in
    order. Both hooks fire for an accepted push and they produce **one** record
    between them — `pre-receive` decides the refusal and writes nothing,
    `post-receive` writes the record — so `hook` reads `"post-receive"` on
    every record this fixture emits today. The field is kept rather than
    implied because a declined push runs only `pre-receive`, and a later
    package recording that arm must be able to say which hook spoke without
    changing a capture shape a tester has already written against.

    `ready_at` is the wall-clock instant at which this push's merge request
    becomes visible (S-022). The hook computes it and exits immediately;
    readiness is evaluated at *read* time. A hook that slept instead would hold
    the CGI child and the HTTP response open for the whole delay.
    """

    hook: str
    invocation: int
    project: str
    options: Sequence[str]
    option_count_present: bool
    option_count: str | None
    updates: Sequence[PushedRef]
    ready_at: float | None


@dataclasses.dataclass(slots=True, frozen=True)
class CredentialHelperCall:
    """One invocation of the fixture `HOME`'s recording `credential.helper`.

    Both halves of C-034 are proved from this: that a credential-injecting run
    resets `credential.helper` and invokes none, and that a push-precedence
    step-3 run invokes the operator's own. Without a helper configured at all,
    "no helper was invoked" is true in every state of the code — including with
    the reset deleted — which is why the helper, not its absence, is the
    fixture.

    `action` is git's own verb (`get`, `store`, `erase`). `stdin` is the
    credential description git wrote, one `key=value` per line; it carries a
    `path=` row because the fixture `HOME` sets `credential.useHttpPath=true`
    (see `build_fixture_home`), which is what lets a test say *which project's*
    credential was asked for rather than only which host.

    `exit_status` is the helper's own. It is **honest capture and not a check**:
    git always passes an operation, so the helper's `0 if action else 2` is `0`
    in every invocation git can produce, and the record is written before the
    exit, so a helper that died earlier leaves no record to carry a non-zero.
    Both states an assertion on it would name are therefore unreachable, and
    `assert calls[0].exit_status == 0` could never go red. It stays on the record
    because a later git — or a non-git caller — can reach the argv-less arm, and
    because `credential_calls() == []` still cannot tell "the code invoked none"
    from "the helper is broken": what separates those is the positive control,
    `::test_credential_helper_records_an_invocation`, and its `len(calls) == 1`.
    """

    action: str
    stdin: str
    exit_status: int


@dataclasses.dataclass(slots=True, frozen=True)
class FixtureHome:
    """A scratch `HOME` for a child `git`, carrying a deliberately wrong identity.

    `~/.gitconfig` reaches the child by design — C-033 allowlists `HOME` so an
    operator's proxy and CA settings apply — so this is the surface that
    discriminates between "ocx set the commit identity" and "git read it from
    `HOME`". `user_name`/`user_email` therefore default to something that is
    **not** `ocx <noreply@ocx.sh>`.

    `credential_log` and `helper` are present only when the home was built with
    `credential_helper=True`.
    """

    path: Path
    gitconfig: Path
    user_name: str
    user_email: str
    helper: Path | None
    credential_log: Path | None

    def credential_calls(self) -> list[CredentialHelperCall]:
        """Every recording-helper invocation, in order; empty when no helper was
        configured or none was invoked.

        Re-read from `credential_log` on each call — the helper is a separate
        process that writes while the test is blocked in `ocx`.

        **An empty answer is not on its own evidence.** It is also what a helper
        git could not execute produces, and git carries on regardless. A test
        whose point is `credential_calls() == []` needs the helper proved
        working somewhere — that is
        `::test_credential_helper_records_an_invocation`.
        """
        if self.credential_log is None or not self.credential_log.exists():
            return []
        calls: list[CredentialHelperCall] = []
        for line in self.credential_log.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            payload = json.loads(line)
            calls.append(
                CredentialHelperCall(
                    action=payload["action"],
                    stdin=payload["stdin"],
                    exit_status=payload["exit"],
                )
            )
        return calls


# ── pure helpers ─────────────────────────────────────────────────────────────

#: The only bytes a chunk-size line may carry. `int(b"0x10", 16)` and
#: `int(b" 10 ", 16)` both parse, so the digits are checked before the parse
#: rather than left to it.
_HEXDIGITS = frozenset(b"0123456789abcdefABCDEF")


def captured_headers(message: EmailMessage) -> dict[str, tuple[str, ...]]:
    """Every header name in `message` mapped to ALL of its values, in order.

    `dict(message.items())` and `{n: v for n, v in message.items()}` both keep
    only the LAST of a repeated name; `message[name]` keeps only the first. Both
    are wrong for `Authorization`, which git repeats whenever more than one
    `http.<prefix>.extraHeader` matches — and the two collapses disagree about
    which one survived, so the same duplicate reads differently on the git log
    and the REST log.

    Keys are the first spelling each name arrived under; values come from
    `get_all`, which matches case-insensitively, so two casings of one name
    yield one entry holding both values rather than two entries holding all of
    them twice.
    """
    # `list(message.keys())`, never `for name in message`: an
    # `email.message.Message` defines no `__iter__`, so bare iteration falls
    # back to `__getitem__(0)` — a lookup for a header literally named `0` — and
    # yields nothing at all. The `list()` is also what tells ruff's SIM118 that
    # this is not a dict.
    spellings: dict[str, str] = {}
    for name in list(message.keys()):
        spellings.setdefault(name.lower(), name)
    return {
        spelling: tuple(message.get_all(lowered) or ())
        for lowered, spelling in spellings.items()
    }


def basic_credential(header: str | None) -> tuple[str, str] | None:
    """The `(user, secret)` pair an HTTP Basic `Authorization` header carries, or
    `None` when it carries no usable one.

    `None` is returned for an absent header, a non-`Basic` scheme, a blob that is
    not valid base64 or not UTF-8, a blob with no `:` at all, and a pair with an
    empty half. The empty halves matter as much as the malformed blobs: RFC 7617
    admits `Basic base64("user:")`, and a credential ladder that fell through to
    an empty secret would authenticate **as nobody** while looking exactly like a
    live push — which is the shape the ladder's own non-emptiness qualifiers
    exist to prevent.

    Partition on the FIRST `:`, as HTTP Basic does: a username carrying one
    re-partitions the pair, and the server then reads a different secret than the
    client resolved.
    """
    if header is None:
        return None
    scheme, _, blob = header.partition(" ")
    if scheme.lower() != "basic" or not blob:
        return None
    try:
        decoded = base64.b64decode(blob.strip(), validate=True).decode("utf-8")
    except (binascii.Error, UnicodeDecodeError):
        return None
    user, separator, secret = decoded.partition(":")
    if not separator or not user or not secret:
        return None
    return user, secret


def _read_crlf_line(rfile: BinaryIO) -> bytes:
    """One CRLF-terminated line, without its terminator.

    A line with no terminator is a stream that ended mid-frame, which is
    `TRUNCATED` — the decoder never returns the bytes it did get.
    """
    line = rfile.readline(_MAX_LINE_BYTES)
    if not line.endswith(b"\n"):
        raise ChunkedBodyError(ChunkedBodyRejection.TRUNCATED)
    return line.rstrip(b"\r\n")


def read_chunked_body(
    rfile: BinaryIO,
    *,
    max_bytes: int = MAX_CHUNKED_BODY_BYTES,
) -> bytes:
    """Decode one RFC 9112 §7.1 chunked request body from a socket stream.

    Deliberately stricter than the grammar, per C-074:

    * a chunk **extension** (`<size>;<ext>`) raises `CHUNK_EXTENSION`;
    * a non-empty **trailer section** after the last chunk raises
      `TRAILER_SECTION`;
    * a decoded body exceeding `max_bytes` raises `OVERSIZED`;
    * a size line that is not `1*HEXDIG` raises `MALFORMED_SIZE`;
    * a stream that ends mid-body raises `TRUNCATED` — it never returns the
      bytes it did get.

    Truncating instead of raising is the failure this strictness exists to
    prevent: a decoder returning the first chunk and dropping the rest makes the
    four-option assertion (C-039) pass or fail for reasons that have nothing to
    do with ocx.
    """
    decoded: list[bytes] = []
    total = 0
    while True:
        size_line = _read_crlf_line(rfile)
        if b";" in size_line:
            raise ChunkedBodyError(ChunkedBodyRejection.CHUNK_EXTENSION)
        if not size_line or any(byte not in _HEXDIGITS for byte in size_line):
            raise ChunkedBodyError(ChunkedBodyRejection.MALFORMED_SIZE)
        size = int(size_line, 16)
        if size == 0:
            break
        total += size
        if total > max_bytes:
            # Refused on the ANNOUNCED length, before the bytes are read: a cap
            # that first buffers what it is about to refuse is not a cap.
            raise ChunkedBodyError(ChunkedBodyRejection.OVERSIZED)
        chunk = rfile.read(size)
        if len(chunk) != size:
            raise ChunkedBodyError(ChunkedBodyRejection.TRUNCATED)
        if rfile.read(2) != b"\r\n":
            raise ChunkedBodyError(ChunkedBodyRejection.TRUNCATED)
        decoded.append(chunk)
    # RFC 9112 allows a trailer section here; C-074 narrows it to the empty one.
    if _read_crlf_line(rfile):
        raise ChunkedBodyError(ChunkedBodyRejection.TRAILER_SECTION)
    return b"".join(decoded)


def split_cgi_response(raw: bytes) -> tuple[int, http.client.HTTPMessage, bytes]:
    """Split a CGI child's stdout into `(status, headers, body)`.

    **The header block is parsed by `http.client.parse_headers` over a
    `BytesIO`, not by hand.** That covers the whole standard part — folding,
    repeated names, case-insensitive lookup, and the bare-LF terminator some CGI
    implementations emit where git 2.54 emits CRLF (verified locally; the docs
    guarantee nothing across versions). Hand-rolling it would own a wire-format
    parser for no reason, which `quality-core.md` § Don't Own Non-Domain Code
    puts at Block tier.

    The one genuine deviation, and the only thing this function owns, is CGI's
    `Status:` demotion (RFC 3875 §6.3.3): a non-2xx answer is signalled by a
    `Status: <code> <reason>` **header line** and by nothing else — `git
    http-backend` exits 0 regardless, so its exit code is never a verdict.
    Absent a `Status:` line the status is 200, and the `Status:` line is
    stripped rather than forwarded as a response header.
    """
    stream = io.BytesIO(raw)
    headers = http.client.parse_headers(stream)
    body = stream.read()
    status_line = headers.get("Status")
    if status_line is None:
        return 200, headers, body
    del headers["Status"]
    return int(status_line.split(None, 1)[0]), headers, body


def cgi_environment(
    *,
    method: str,
    path_info: str,
    query_string: str,
    headers: Mapping[str, Sequence[str]],
    project_root: Path,
    content_length: int | None,
    remote_user: str,
    remote_addr: str,
) -> dict[str, str]:
    """Build the CGI environment block for one `git http-backend` invocation.

    **Every** entry of `headers` is mapped to `HTTP_<NAME>` with dashes turned
    into underscores and the name upper-cased — `Git-Protocol` is not special
    -cased, it merely arrives. A bridge that passes a curated set silently
    downgrades every request to protocol v0. `headers` is the multi-valued map
    `captured_headers` builds; a name sent more than once is joined with `", "`,
    which is what RFC 3875 §4.1.18 requires and is also the only form that does
    not silently pick one of two `Authorization` headers.

    **Accepted risk, named rather than left unremarked: forwarding everything
    means a `Proxy:` request header becomes `HTTP_PROXY` in the child's
    environment — the httpoxy shape (CVE-2016-5385 and family).** It is not
    exploitable here and forwarding is not negotiable: C-034 and S-040 are both
    asserted against what the bridge passed on, so a curated set would make the
    thing under test unobservable. What bounds it: the server binds loopback
    (`fake_forge.py`), the test process is the only client, `git http-backend`
    makes no outbound HTTP request of its own, and an injected name can only
    ever produce a variable beginning `HTTP_`, so no other CGI variable can be
    forged through this path. A future bridge that shells out to something which
    *does* dial out must revisit this line before it does.

    `CONTENT_LENGTH` is set from `content_length`, which for a chunked request is
    the length **after** decoding. `http-backend` reads exactly that many bytes
    from stdin, so handing it the on-the-wire framing length would truncate or
    hang the RPC.

    `GIT_HTTP_EXPORT_ALL` is set (no `git-daemon-export-ok` marker is written),
    and `GIT_PROJECT_ROOT` anchors repository lookup at `project_root`.
    `REMOTE_USER` is carried because `receive-pack` derives the reflog identity
    from it.
    """
    env = {
        "GATEWAY_INTERFACE": "CGI/1.1",
        "SERVER_PROTOCOL": "HTTP/1.1",
        "SERVER_SOFTWARE": "ocx-fake-forge/1.0",
        "REQUEST_METHOD": method,
        "PATH_INFO": path_info,
        "QUERY_STRING": query_string,
        "GIT_PROJECT_ROOT": str(project_root),
        "GIT_HTTP_EXPORT_ALL": "1",
        "REMOTE_USER": remote_user,
        "REMOTE_ADDR": remote_addr,
    }
    if content_length is not None:
        env["CONTENT_LENGTH"] = str(content_length)
    for name, values in headers.items():
        value = ", ".join(values)
        env["HTTP_" + name.upper().replace("-", "_")] = value
        if name.lower() == "content-type":
            # RFC 3875 §4.1.3 spells this one without the `HTTP_` prefix, and
            # `http-backend`'s `check_content_type` reads exactly that name: a
            # bridge forwarding only the prefixed form makes every RPC answer
            # "Bad Request" for a reason no assertion mentions.
            env["CONTENT_TYPE"] = value
    return env


def git_environment(
    home: Path, *, extra: Mapping[str, str] | None = None
) -> dict[str, str]:
    """The hygienic environment every fixture-side `git` invocation runs under.

    `GIT_CONFIG_NOSYSTEM=1` and a scratch `HOME` keep the developer's own
    `/etc/gitconfig` and `~/.gitconfig` out of the fixture — a host
    `init.defaultBranch`, `core.hooksPath` or `commit.gpgsign` would otherwise
    change what the fixture builds. `LC_ALL=C` and `LANGUAGE=` pin git's message
    language, `GIT_TERMINAL_PROMPT=0` stops a credential prompt wedging a run,
    and `PATH` is carried so `git` itself resolves.

    This is the *fixture's* environment. It is not C-035's table and must not be
    confused with it: C-035 describes the environment **ocx** builds for its own
    child, which is the thing under test.
    """
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_TERMINAL_PROMPT": "0",
        "LC_ALL": "C",
        "LANGUAGE": "",
    }
    if extra is not None:
        env.update(extra)
    return env


def run_git(
    *args: str,
    home: Path,
    cwd: Path | None = None,
    extra_env: Mapping[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run one raw `git` invocation against a chosen environment — the replay
    helper.

    This is the only route to C-034's "again through git's own URL
    normalisation": ocx builds exactly one `http.<prefix>.extraHeader` per run,
    so trailing slash, uppercase host, an explicit port and a sibling project
    path are four **replays** of the same URL, not four ocx runs. Note the
    server binds `127.0.0.1` (`fake_forge.py:343`), so the variant set is built
    from the bound address and the uppercase-host variant has no IP-literal
    form.

    It is also how chunked framing is forced: pass `-c http.postBuffer=1` so a
    push that would otherwise fit inside the 1 MiB default switches to
    `Transfer-Encoding: chunked`.

    Output is captured as text; `check=True` raises `GitFixtureError` carrying
    stderr, because a silently failed setup step turns every later assertion
    into a measurement of the fixture.
    """
    try:
        completed = subprocess.run(
            ["git", *args],
            cwd=None if cwd is None else str(cwd),
            env=git_environment(home, extra=extra_env),
            capture_output=True,
            text=True,
            timeout=_GIT_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired as timeout:
        raise GitFixtureError(
            f"git {' '.join(args)} did not finish within {_GIT_TIMEOUT_SECONDS}s"
        ) from timeout
    if check and completed.returncode != 0:
        raise GitFixtureError(
            f"git {' '.join(args)} failed ({completed.returncode})\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return completed


def _run_git_bytes(*args: str, home: Path) -> bytes:
    """One `git` invocation whose stdout is raw bytes — blob content.

    Separate from `run_git` rather than a flag on it: `run_git` is the replay
    helper tests drive and its output is text by contract, and decoding a blob
    through that path would corrupt any file that is not UTF-8.
    """
    completed = subprocess.run(
        ["git", *args],
        env=git_environment(home),
        capture_output=True,
        timeout=_GIT_TIMEOUT_SECONDS,
        check=False,
    )
    if completed.returncode != 0:
        raise GitFixtureError(
            f"git {' '.join(args)} failed ({completed.returncode}): "
            f"{completed.stderr.decode(errors='replace')}"
        )
    return completed.stdout


def create_bare_repository(
    root: Path,
    project_path: str,
    *,
    home: Path,
    config: Mapping[str, str] = BARE_REPO_CONFIG,
    initial_branch: str = INITIAL_BRANCH,
) -> Path:
    """`git init --bare -b <initial_branch>` at `<root>/<project_path>.git`,
    then apply `config` row by row.

    `config` is a parameter rather than a constant read inside so a test can drop
    exactly one row and observe what stops working — `uploadpack.allowFilter` is
    the row whose absence is otherwise invisible, since the fetch still succeeds
    and only the missing-object count changes.

    Runs under `git_environment(home)`, so nothing about the developer's own git
    configuration reaches the repository being built.
    """
    repository = root / f"{project_path}.git"
    repository.parent.mkdir(parents=True, exist_ok=True)
    run_git("init", "--bare", "-b", initial_branch, str(repository), home=home)
    for key, value in config.items():
        run_git("-C", str(repository), "config", key, value, home=home)
    return repository


def build_fixture_home(
    root: Path,
    *,
    user_name: str = FIXTURE_HOME_USER_NAME,
    user_email: str = FIXTURE_HOME_USER_EMAIL,
    credential_helper: bool = False,
    post_buffer: int | None = None,
) -> FixtureHome:
    """Build a scratch `HOME` under `root` for a child `git` to read (DX-6).

    Writes `~/.gitconfig` with `user.name`/`user.email` set to an identity that
    is **not** ocx's, so C-045's commit-identity assertion discriminates rather
    than agreeing with itself.

    `credential_helper=True` installs a recording `credential.helper` that
    appends its action and stdin to `FixtureHome.credential_log` and answers
    nothing. Both halves of C-034 need it: on a credential-injecting run the
    helper must be reset and stay unrecorded, and on a push-precedence step-3
    run it must be invoked. With no helper configured the first half is true in
    every state of the code, including with the reset deleted. The helper's
    own liveness is proved by `::test_credential_helper_records_an_invocation`
    — without it, "no helper was invoked" is equally true of a helper git could
    not execute, and git says nothing when that happens.

    It writes `credential.useHttpPath = true` alongside. git otherwise strips
    the path from what it hands a helper, keying credentials by host alone, and
    the recorded stdin then names only `127.0.0.1:<port>` — enough for "some
    credential was requested", not for "the *index project's* credential was
    requested", which is the question S-040 and C-034's scoping half ask. It
    changes no behaviour here: nothing answers, so nothing is looked up.

    `post_buffer` writes `http.postBuffer`, which is how a small push is forced
    onto the chunked path (C-074) from the `HOME` side rather than per
    invocation. The value is written verbatim, and two properties of git 2.54.0
    bound the useful range — both measured, not assumed:

    * a `HOME` carrying **anything at or below `LARGE_PACKET_MAX` (65520)**
      aborts every protocol-v2 fetch with `BUG: remote-curl.c: The entire
      rpc->buf should be larger than LARGE_PACKET_MAX`, so a clone under such a
      `HOME` dies before any push happens. The per-invocation route
      (`run_git("-c", "http.postBuffer=1", "push", …)`) is unaffected: a push
      speaks v0 and never reaches that assertion.
    * git streams chunked when the request exceeds `post_buffer - 65520`, not
      `post_buffer`. **65536** is therefore the smallest `HOME`-side value that
      both survives a clone and forces a claim-sized push onto chunked framing;
      the 1 MiB default leaves a ~983 KiB threshold, which no claim ever
      crosses.
    """
    root.mkdir(parents=True, exist_ok=True)
    helper: Path | None = None
    credential_log: Path | None = None
    sections = [f"[user]\n\tname = {user_name}\n\temail = {user_email}\n"]
    if credential_helper:
        credential_log = root / "credential-helper.jsonl"
        helper = root / "credential-helper.py"
        helper.write_text(
            _CREDENTIAL_HELPER_SOURCE.format(
                python=sys.executable, log=repr(str(credential_log))
            ),
            encoding="utf-8",
        )
        helper.chmod(0o755)
        # An absolute path is run directly by git, with the action as argv[1];
        # a bare word would be looked up as `git credential-<word>`.
        # `useHttpPath` keeps the project path in the description git writes to
        # the helper's stdin — without it a recorded call names the host only.
        sections.append(f"[credential]\n\thelper = {helper}\n\tuseHttpPath = true\n")
    if post_buffer is not None:
        sections.append(f"[http]\n\tpostBuffer = {post_buffer}\n")
    gitconfig = root / ".gitconfig"
    gitconfig.write_text("".join(sections), encoding="utf-8")
    return FixtureHome(
        path=root,
        gitconfig=gitconfig,
        user_name=user_name,
        user_email=user_email,
        helper=helper,
        credential_log=credential_log,
    )


# ── the mixin ────────────────────────────────────────────────────────────────


class GitHttpRoutes:
    """Smart-HTTP git routes, mixed into `FakeForge`.

    State lives flat on the server under a `git_http_` prefix, exactly as the
    GitLab surface's state lives under `gitlab_` — one vocabulary, not two. The
    knobs mirror the shapes already established on the REST side:
    `git_http_concurrent_advance` is `concurrent_ref_advance`
    (`fake_forge.py:297`) for the git transport, and `git_http_redirect_next` is
    `redirect_next_contents` (`:287`).

    Every knob defaults to today's behaviour, so a test that sets nothing
    behaves exactly as before this module existed.

    **Coupling, declared in both directions** (no `Protocol` — there is exactly
    one host, and an interface with one implementation is scaffolding):

    * This mixin requires the host to be `FakeForge`. It reads `lock`, `refs`,
      `repos`, `trees`, `blobs`, `commits`, `base_url` and `token_identity_login`,
      and it registers imported commits through `_record_commit_locked`.
    * `fake_gitlab.GitLabRoutes.gl_get_merge_requests` requires **this** mixin on
      the host: it calls `git_promote_ready_merge_requests_locked` under the
      lock. A `FakeForge` built without `GitHttpRoutes` raises `AttributeError`
      on every merge-request poll.
    """

    # ── lifecycle ────────────────────────────────────────────────────────

    def git_http_init(self) -> None:
        """Initialise the git-transport state. Called from `FakeForge.__init__`
        **before** the socket is bound.

        Creates no directory: the project root is materialised on first
        `git_create_project`, so a server no test pushes to costs nothing and
        leaves nothing behind.

        Implemented rather than stubbed, for the reason C-017 gives for
        `ForgeKind::client`: `FakeForge.__init__` runs for all six existing
        consumer modules, so a raising body here fails every one of them.
        Nothing below is logic — it is the same flat knob block the `gitlab_*`
        surface already keeps in `__init__`.
        """
        self._git_http_root: Path | None = None
        self._git_http_scratch_home: Path | None = None
        #: Push invocation numbers already turned into a merge request, so a
        #: second poll does not open a second request for the same push.
        self._git_http_promoted: set[int] = set()
        #: Registered projects: `"<owner>/<repo>"` -> bare repository path. This
        #: mapping IS the route predicate — an unregistered project is not a git
        #: path, whatever it looks like — which is what keeps every REST request
        #: of every existing consumer out of this module.
        self.git_http_projects: dict[str, Path] = {}
        #: Per-request capture (see `GitRequest`). The git transport's own log,
        #: never `requests`.
        self.git_http_requests: list[GitRequest] = []
        #: Ceiling on a decoded chunked body; lowered by a test so the cap is
        #: reachable without sending megabytes.
        self.git_http_max_body_bytes: int = MAX_CHUNKED_BODY_BYTES
        #: How long the bridge waits for the rest of a request body before it
        #: abandons the connection — both framings. Lowered by a test for the
        #: same reason as the ceiling above: at the 30s default no bounded test
        #: can watch the abandonment happen, and an unobservable guard is one
        #: nobody notices losing.
        self.git_http_body_read_timeout_seconds: float = BODY_READ_TIMEOUT_SECONDS
        #: `"<project>/<branch>"` -> files a racing writer lands with real `git`
        #: immediately before the receive-pack **ref advertisement**, producing
        #: a genuine non-fast-forward and a genuine `(stale info)`. Fires ONCE
        #: then clears — that is what proves C-043's retry converges rather than
        #: merely runs. The git-transport twin of `concurrent_ref_advance`.
        #:
        #: Before the advertisement, not before the POST: git decides both
        #: `(fetch first)` and `(stale info)` CLIENT-side against the refs that
        #: response carries. A commit landed after it is refused by
        #: `receive-pack` as `incorrect old value provided` instead — a
        #: different refusal, and not one C-044 classifies. Verified against
        #: git 2.54.0.
        #:
        #: This is the ONE bare-repo mutation that is neither a seed nor a push,
        #: so it is the one place DX-5's two stores can silently drift: the
        #: racing commit MUST be re-imported through `git_import_ref` after it
        #: lands, or the REST surfaces keep answering the pre-race sha and the
        #: retry's `--force-with-lease` is measured against a fixture artefact.
        self.git_http_concurrent_advance: dict[str, dict[str, bytes]] = {}
        #: The literal line the `pre-receive` hook writes to stderr before
        #: declining. Written WITHOUT a `remote: ` prefix — `send-pack` adds
        #: that itself, so a hook echoing the quoted string verbatim produces
        #: `remote: remote: …` and an exact-line assertion reds for the wrong
        #: reason. `None` accepts.
        self.git_http_pre_receive_refusal: str | None = None
        #: Push-option KEYS the `pre-receive` hook declines on, e.g.
        #: `{"merge_request.merge_when_pipeline_succeeds"}`. The hook reads the
        #: received `GIT_PUSH_OPTION_<n>` values, splits each on the first `=`,
        #: and declines with the same no-`remote: `-prefix rule above when any
        #: key is in this set.
        #:
        #: Distinct from the unconditional knob because S-013's error case is "a
        #: fifth or substituted option key -> the fixture's hook fails the test".
        #: With only the unconditional knob the hook declines every push or none,
        #: so `::test_merge_when_pipeline_succeeds_is_rejected_by_the_hook`
        #: degrades into re-reading `PushRecord.options` — which is already
        #: `::test_push_delivers_exactly_the_four_option_keys`, i.e. one
        #: assertion counted twice rather than the server-side refusal C-039
        #: names.
        self.git_http_forbidden_push_options: set[str] = set()
        #: 403 on the NEXT `GET /info/refs?service=git-receive-pack`, then
        #: cleared. Armed separately from the POST because the two produce
        #: different client-side stderr and C-044's phrase table does not
        #: distinguish them.
        #:
        #: Fires ONCE, like every other scripted failure in this fixture family
        #: (`tree_fail_once`, `pull_fail_once`, `redirect_next_contents`).
        #: `::test_server_side_rejection_texts_are_recorded` drives all six
        #: refusal shapes against ONE server and resets nothing between them, so
        #: a knob that stayed armed would answer 403 to the `receive-pack` arm
        #: that follows it and every later shape would measure this one instead.
        #: A test that needs a persistently unpushable project re-arms it per
        #: push rather than relying on a latch nothing clears — there is no
        #: sticky mode and adding one would break the six-shape producer.
        self.git_http_forbid_info_refs: bool = False
        #: 403 on the next `POST /git-receive-pack` **that carries a pack**,
        #: then cleared — same one-shot rule and same re-arm-per-push
        #: convention as the knob above.
        #:
        #: "that carries a pack" is the whole subtlety. A chunked push sends
        #: TWO receive-pack POSTs: git emits a `probe_rpc` first — a 4-byte
        #: `0000` flush-packet body under `Content-Length`
        #: (`_RECEIVE_PACK_PROBE_BODY`) — because a chunked stream cannot be
        #: replayed after a 401, so the auth handshake has to finish before the
        #: stream starts. Firing on the literal first POST therefore refuses the
        #: probe, and the pack POST never happens at all: the knob then means
        #: "the handshake was refused" under chunked framing and "the pack was
        #: refused" under `Content-Length`, which is the test's choice of
        #: framing deciding what the fixture measured.
        #:
        #: Measured, not assumed, against git 2.54.0: `RPC failed; HTTP 403` —
        #: the phrase C-044's table names — comes out either way, so this is not
        #: about the phrase. It is about the refusal being the one a WP-17
        #: assertion will believe it observed.
        self.git_http_forbid_receive_pack: bool = False
        #: `Location` for the NEXT `GET /info/refs`, then cleared — the
        #: git-transport twin of `redirect_next_contents`. Build the value with
        #: `git_url` so the target is a real sibling project: a synthetic path
        #: could not match the credential's URL prefix under any implementation,
        #: which would make "no `Authorization` reached it" true by construction.
        self.git_http_redirect_info_refs: str | None = None
        #: `Location` for the NEXT `POST /git-receive-pack`, then cleared.
        #:
        #: Split from the `info/refs` knob because a single "next git request"
        #: knob can never reach this shape: a push's FIRST request is
        #: `GET /info/refs?service=git-receive-pack`, and C-068's
        #: `followRedirects=false` aborts the push right there — so the
        #: POST-redirect S-039 actually names would be unreachable and the test
        #: would silently prove the `info/refs` case twice.
        self.git_http_redirect_receive_pack: str | None = None
        #: Seconds between an accepted push and its merge request becoming
        #: visible. `0.0` is immediate, a positive value lands inside the poll
        #: bound, and `None` never becomes ready (S-022's exit 75).
        self.git_http_merge_request_delay: float | None = 0.0
        #: The ONE `(user, secret)` pair this server accepts, or `None` for "any
        #: well-formed HTTP Basic pair with two non-empty halves on the write
        #: half, and anonymous reads".
        #:
        #: The default is what makes a push prove a credential was sent at all —
        #: see the module docstring. This knob is what makes a **wrong** one
        #: reachable: the bridge has no other way to know which secret is the
        #: right one, since every consumer resolves its own, so "the client
        #: surfaced the server's refusal" has no state to be tested in without
        #: it. Persistent rather than one-shot, unlike the 403 knobs: a bad
        #: credential is a property of the run, not a scripted single failure,
        #: and git retries the handshake within one push.
        #:
        #: Applies to the write half always, and to the read half only when
        #: `git_http_private` says so.
        self.git_http_credential: tuple[str, str] | None = None
        #: Whether the READ half demands a credential too — a private project.
        #:
        #: Off by default, and the default is the load-bearing case: GitLab
        #: serves a **public** project's `upload-pack` to anyone, so a run whose
        #: credential the forge rejects fetches successfully and is refused at
        #: the *push*. That is the shape production actually meets, and a fixture
        #: that could only reject on the fetch would leave it untested.
        #:
        #: On, it reaches the other shape: the refusal lands at
        #: `GitWorkspace::open`'s fetch, before anything is built. Both are
        #: needed because ocx classifies them through different tables —
        #: `credential_rejection_status` for the fetch, `classify_push_failure`
        #: for the push.
        self.git_http_private: bool = False
        #: Answer **403** to every `upload-pack` request, however well the
        #: credential authenticates. Persistent, for the reason above.
        #:
        #: The authorization half of the gate, and a different refusal from the
        #: one above: 401 says "this is not a credential I know", 403 says "it is,
        #: and it may not read this project". git prints libcurl's `The requested
        #: URL returned error: 403` for the second and `Authentication failed
        #: for` for the first, which is exactly the distinction
        #: `git_stderr.rs`'s fetch table recovers a status from — so a build that
        #: collapsed the two would leave one of its two needles unreachable.
        self.git_http_forbid_fetch: bool = False

    def git_http_cleanup(self) -> None:
        """Remove the scratch project root **and the scratch `HOME`**, if either
        was ever created. Called from `FakeForge.server_close`; a no-op when no
        git project exists.

        Both, and independently, because the two are separate lazily created
        directories and nothing here fixes their relative layout: removing only
        the root leaks one directory per test if the Implement phase puts the
        `HOME` beside it rather than inside it, and that leak is invisible —
        `/tmp` fills up in some later, unrelated run.

        Implemented for the same reason as `git_http_init`: every existing
        consumer's fixture teardown reaches it.

        **No `ignore_errors`.** That flag swallows exactly the failure this
        method's own docstring is written against — a directory that could not
        be removed is the leak, and suppressing the error makes the leak
        invisible in the one place that could have reported it. A removal that
        genuinely cannot succeed should fail the teardown loudly.
        """
        root = self._git_http_root
        home = self._git_http_scratch_home
        self._git_http_root = None
        self._git_http_scratch_home = None
        for path in (root, home):
            if path is not None:
                shutil.rmtree(path)

    @property
    def git_project_root(self) -> Path:
        """`GIT_PROJECT_ROOT` — the directory holding `<owner>/<repo>.git`,
        created on first use."""
        return self._git_http_subdirectory("projects")

    @property
    def _git_http_state(self) -> Path:
        """The scratch directory holding the hook config and the push log.

        Beside `projects/` rather than inside it: `GIT_PROJECT_ROOT` is what
        `http-backend` resolves a request path against, and the fixture's own
        bookkeeping has no business being addressable over HTTP.
        """
        return self._git_http_subdirectory("state")

    def _git_http_subdirectory(self, name: str) -> Path:
        """One directory under the lazily created scratch root.

        Nothing is created until a project is, so a server no test pushes to
        costs nothing and leaves nothing behind.
        """
        root = self._git_http_root
        if root is None:
            root = Path(tempfile.mkdtemp(prefix="ocx-fake-forge-git-"))
            self._git_http_root = root
        directory = root / name
        directory.mkdir(parents=True, exist_ok=True)
        return directory

    @property
    def git_scratch_home(self) -> Path:
        """The `HOME` the **fixture's own** `git` invocations run under.

        Distinct from `build_fixture_home`, which builds the `HOME` given to the
        process under test and deliberately carries a wrong identity. This one
        is neutral: it exists only so repository setup is not steered by the
        developer's `~/.gitconfig`.
        """
        home = self._git_http_scratch_home
        if home is None:
            home = Path(tempfile.mkdtemp(prefix="ocx-fake-forge-home-"))
            (home / ".gitconfig").write_text(
                "[user]\n\tname = OCX Fixture\n\temail = fixture@example.invalid\n",
                encoding="utf-8",
            )
            self._git_http_scratch_home = home
        return home

    # ── dispatch (called before `FakeForge.record`) ───────────────────────

    def git_http_get(self, handler: _Handler, path: str, query_string: str) -> bool:
        """Serve a git GET, returning False when the path is not a git route.

        `query_string` is the **raw** query, not `parse_qs`'d: `QUERY_STRING`
        reaches `http-backend` verbatim, and re-encoding a parsed mapping is
        lossy.

        A path is a git route only when its project segment names a repository
        created through `git_create_project`, so a test that creates none can
        never divert a REST request into this branch.

        The empty-registry fast path is implemented, not stubbed: this runs on
        **every** GET of all six existing consumer modules.
        """
        if not self.git_http_projects:
            return False
        route = self._git_http_route(path)
        if route is None:
            return False
        project, rest = route
        service = (urllib.parse.parse_qs(query_string).get("service") or [""])[0]

        # BEFORE every knob below, because each of those is ONE-SHOT: a refused
        # request that spent `git_http_concurrent_advance` or a 403 latch would
        # leave the test's armed failure already consumed, and the run it was
        # armed for would then see the unscripted path.
        if not self._git_http_authorize(
            handler,
            project=project,
            method="GET",
            path=path,
            query_string=query_string,
            service=service,
        ):
            return True

        if rest == "/info/refs":
            if service == "git-receive-pack":
                # BEFORE the advertisement, not before the POST: `(fetch first)`
                # and `(stale info)` are both decided CLIENT-side against the
                # refs this response carries. Landing the racing commit after
                # the advertisement instead produces `incorrect old value
                # provided`, which is a different refusal and not the one C-044
                # classifies.
                self._git_http_apply_concurrent_advance(project)
            location = self.git_http_redirect_info_refs
            if location is not None:
                self.git_http_redirect_info_refs = None
                self._git_http_reply(
                    handler,
                    project=project,
                    method="GET",
                    path=path,
                    query_string=query_string,
                    service=service,
                    status=302,
                    body=b"",
                    extra_headers={"Location": location},
                )
                return True
            if self.git_http_forbid_info_refs and service == "git-receive-pack":
                self.git_http_forbid_info_refs = False
                self._git_http_reply(
                    handler,
                    project=project,
                    method="GET",
                    path=path,
                    query_string=query_string,
                    service=service,
                    status=403,
                    body=b"forbidden\n",
                )
                return True

        # Everything else — `info/refs` for upload-pack, `HEAD`, and the dumb
        # `objects/**` paths — goes to `http-backend`, which refuses the dumb
        # ones itself because `http.getanyfile` is false in `BARE_REPO_CONFIG`.
        self._git_http_backend(
            handler,
            project=project,
            method="GET",
            path=path,
            query_string=query_string,
            service=service,
            body=b"",
            framing=BodyFraming.NONE,
        )
        return True

    def git_http_post(self, handler: _Handler, path: str, query_string: str) -> bool:
        """Serve a git POST (`git-upload-pack` / `git-receive-pack`), returning
        False when the path is not a git route.

        Dispatched from `_Handler.do_POST` **before** `record()` and before
        `_read_body()`: the body is pack data, and `_read_body` would consume it
        as JSON and hand the CGI child an empty stdin.

        The armed server-side ref advance (`git_http_concurrent_advance`) is
        NOT applied here but on the receive-pack `info/refs` GET — see
        `_git_http_apply_concurrent_advance`. Both `(fetch first)` and
        `(stale info)` are decided by the CLIENT against the ref advertisement,
        so a racing commit that lands after it produces `incorrect old value
        provided` instead, which is a different refusal and not one C-044
        classifies.

        The empty-registry fast path is implemented, not stubbed: this runs on
        **every** POST of all six existing consumer modules.
        """
        if not self.git_http_projects:
            return False
        route = self._git_http_route(path)
        if route is None:
            return False
        project, rest = route
        service = rest.lstrip("/")

        # The body is read BEFORE any refusal so a 403 or a redirect leaves no
        # unread bytes on a keep-alive socket — those would frame the NEXT
        # request and the failure would surface inside an unrelated test.
        try:
            framing, body = self._git_http_read_body(handler)
        except ChunkedBodyError as refused:
            self._git_http_reply(
                handler,
                project=project,
                method="POST",
                path=path,
                query_string=query_string,
                service=service,
                status=400,
                body=f"{refused.reason.value}\n".encode(),
                framing=refused.framing,
                rejection=refused.reason,
                close=True,
            )
            return True
        except (TimeoutError, OSError):
            # The client stopped mid-body. Nothing to answer to; drop the
            # connection rather than leak the handler thread for the session.
            handler.close_connection = True
            return True

        # After the body read, which every refusal on this path is (the unread
        # bytes would frame the NEXT request on a keep-alive socket), and before
        # every one-shot knob below, for the reason the `info/refs` arm gives.
        if not self._git_http_authorize(
            handler,
            project=project,
            method="POST",
            path=path,
            query_string=query_string,
            service=service,
            framing=framing,
            body_bytes=len(body),
        ):
            return True

        if service == "git-receive-pack":
            location = self.git_http_redirect_receive_pack
            if location is not None:
                self.git_http_redirect_receive_pack = None
                self._git_http_reply(
                    handler,
                    project=project,
                    method="POST",
                    path=path,
                    query_string=query_string,
                    service=service,
                    status=302,
                    body=b"",
                    framing=framing,
                    body_bytes=len(body),
                    extra_headers={"Location": location},
                )
                return True
            carries_pack = body not in (b"", _RECEIVE_PACK_PROBE_BODY)
            if self.git_http_forbid_receive_pack and carries_pack:
                self.git_http_forbid_receive_pack = False
                self._git_http_reply(
                    handler,
                    project=project,
                    method="POST",
                    path=path,
                    query_string=query_string,
                    service=service,
                    status=403,
                    body=b"forbidden\n",
                    framing=framing,
                    body_bytes=len(body),
                )
                return True
            self._git_http_write_hook_config()

        self._git_http_backend(
            handler,
            project=project,
            method="POST",
            path=path,
            query_string=query_string,
            service=service,
            body=body,
            framing=framing,
        )
        if service == "git-receive-pack":
            # DX-5: whatever the push landed is imported under its REAL sha, so
            # the REST surfaces answer what the bare repository actually holds.
            self._git_http_import_all_refs(project)
        return True

    # ── bridge internals ──────────────────────────────────────────────────

    def _git_http_route(self, path: str) -> tuple[str, str] | None:
        """`(project, remainder)` when `path` names a REGISTERED project.

        The registry is the route predicate — an unregistered project is not a
        git path, whatever it looks like — which is what keeps every REST
        request of every existing consumer out of this module. Longest project
        first, so `ocx-sh/index` cannot swallow a request for
        `ocx-sh/index-mirror`.
        """
        trimmed = path.lstrip("/")
        for project in sorted(self.git_http_projects, key=len, reverse=True):
            for candidate in (f"{project}.git", project):
                if trimmed == candidate:
                    return project, ""
                if trimmed.startswith(f"{candidate}/"):
                    return project, trimmed[len(candidate) :]
        return None

    def _git_http_authorize(
        self,
        handler: _Handler,
        *,
        project: str,
        method: str,
        path: str,
        query_string: str,
        service: str,
        framing: BodyFraming = BodyFraming.NONE,
        body_bytes: int = 0,
    ) -> bool:
        """Whether this request may be served, answering **401** or **403**
        itself when it may not.

        **The one gate every git route passes through**, called once from
        `git_http_get` and once from `git_http_post` rather than per route: a
        server that guarded the RPC and left the advertisement open lets a
        credential-less run get as far as deciding `(fetch first)` client-side,
        and that refusal is a different one from the auth failure the run
        deserves. The same argument applies one level up — guarding the write
        half and leaving the read half open makes a rejected credential
        unobservable through ocx entirely, because a write ocx cannot
        authenticate never starts.

        Two refusals, as GitLab answers them:

        * **401 + `WWW-Authenticate`** when the credential is missing, malformed,
          empty-halved, or simply not the accepted secret. GitLab answers a bad
          token this way — `HTTP Basic: Access denied` — not 403, and git's own
          `Authentication failed for` needle is downstream of it.
        * **403** when the credential authenticates and is still not allowed to
          read the project (`git_http_forbid_fetch`).

        Authentication before authorization, so a request carrying nothing gets
        the challenge rather than the flat refusal.

        The read half is served anonymously unless `git_http_private` closes it —
        a public GitLab project advertises its refs to anyone, and a bridge
        demanding a credential for every clone would model a different server
        from the one this suite is about. Which half refuses decides which of
        ocx's two classification tables the run goes through, so both states are
        reachable rather than one being the fixture's opinion.

        `handler.headers.get` takes the FIRST `Authorization` of a repeated name,
        which is what a real server does. Whether a second one arrived at all is
        a separate question, and it is asked where it belongs — on the recorded
        `GitRequest.header_values`, which keeps every value.
        """
        writing = service == "git-receive-pack"
        expected = self.git_http_credential
        if writing or self.git_http_private:
            credential = basic_credential(handler.headers.get("Authorization"))
            if credential is None or (expected is not None and credential != expected):
                self._git_http_reply(
                    handler,
                    project=project,
                    method=method,
                    path=path,
                    query_string=query_string,
                    service=service,
                    status=401,
                    body=b"unauthorized\n",
                    framing=framing,
                    body_bytes=body_bytes,
                    extra_headers={"WWW-Authenticate": _BASIC_CHALLENGE},
                )
                return False
        if not writing and self.git_http_forbid_fetch:
            self._git_http_reply(
                handler,
                project=project,
                method=method,
                path=path,
                query_string=query_string,
                service=service,
                status=403,
                body=b"forbidden\n",
                framing=framing,
                body_bytes=body_bytes,
            )
            return False
        return True

    def _git_http_read_body(self, handler: _Handler) -> tuple[BodyFraming, bytes]:
        """The request body and the framing it arrived under.

        `BaseHTTPRequestHandler` decodes nothing: `rfile` is the raw socket
        stream, so a chunked body is decoded here (C-074) and a
        `Content-Length` one is read outright.

        **Both framings get the same timeout, the same ceiling, and the same
        refusal.** A `Content-Length` body is no less client-controlled than a
        chunked one: `rfile.read(n)` blocks until `n` bytes arrive, so a
        half-sent `Content-Length` request holds the handler thread exactly as a
        half-sent chunked one does, and a declared length above the ceiling
        buffers what the chunked path refuses on sight. A negative length is the
        sharpest of the three — `read(-1)` reads to EOF, which is unbounded in
        both time and memory.

        All three RAISE, where an earlier version clamped the declared length
        into `[0, git_http_max_body_bytes]` and read a prefix. Clamping is the
        worse failure: it answers a truncated body as if it were whole, and the
        bytes it did not read then frame the NEXT request on a keep-alive
        socket — so the damage surfaces inside an unrelated test. An unparsable
        length clamped to `0` is the same bug with a bigger silence. The module
        docstring's "the fixture must not be able to wedge a run" is the
        invariant all three defend.
        """
        timeout = self.git_http_body_read_timeout_seconds
        transfer = handler.headers.get("Transfer-Encoding", "") or ""
        if "chunked" in transfer.lower():
            handler.connection.settimeout(timeout)
            try:
                return BodyFraming.CHUNKED, read_chunked_body(
                    handler.rfile, max_bytes=self.git_http_max_body_bytes
                )
            finally:
                handler.connection.settimeout(None)
        length = handler.headers.get("Content-Length")
        if length is None:
            return BodyFraming.NONE, b""
        try:
            declared = int(length)
        except ValueError as unparsable:
            raise ChunkedBodyError(
                ChunkedBodyRejection.MALFORMED_SIZE, BodyFraming.CONTENT_LENGTH
            ) from unparsable
        if declared < 0:
            raise ChunkedBodyError(
                ChunkedBodyRejection.MALFORMED_SIZE, BodyFraming.CONTENT_LENGTH
            )
        if declared > self.git_http_max_body_bytes:
            # Refused on the DECLARED length, before a byte is read — the same
            # rule `read_chunked_body` applies to an announced chunk size.
            raise ChunkedBodyError(
                ChunkedBodyRejection.OVERSIZED, BodyFraming.CONTENT_LENGTH
            )
        handler.connection.settimeout(timeout)
        try:
            return BodyFraming.CONTENT_LENGTH, handler.rfile.read(declared)
        finally:
            handler.connection.settimeout(None)

    def _git_http_reply(
        self,
        handler: _Handler,
        *,
        project: str,
        method: str,
        path: str,
        query_string: str,
        service: str,
        status: int,
        body: bytes,
        framing: BodyFraming = BodyFraming.NONE,
        body_bytes: int = 0,
        rejection: ChunkedBodyRejection | None = None,
        extra_headers: Mapping[str, str] | None = None,
        close: bool = False,
        cgi_env: Mapping[str, str] | None = None,
        backend_stderr: str = "",
        response_headers: Sequence[tuple[str, str]] | None = None,
    ) -> None:
        """Write one fixture-authored response and record it.

        `Content-Length` is always set. `git http-backend` streams without one,
        and under this fixture's HTTP/1.1 keep-alive an unframed response hangs
        the client — with no `pytest-timeout` to rescue the run.

        The record is appended **before** the response is written. Every test
        here slices `git_http_requests` the moment its client call returns, and
        a record written after the last byte goes out is a record the client can
        outrun — a refused body loses the race by the whole drain interval.
        `list.append` is atomic under the GIL, which is the only synchronisation
        this log needs.
        """
        self.git_http_requests.append(
            GitRequest(
                method=method,
                path=path,
                query=query_string,
                project=project,
                service=service,
                headers=captured_headers(handler.headers),
                framing=framing,
                body_bytes=body_bytes,
                status=status,
                cgi_env=dict(cgi_env or {}),
                backend_stderr=backend_stderr,
                rejection=rejection,
            )
        )
        try:
            handler.send_response(status)
            # Header PAIRS, not a mapping: a name the child sent twice must be
            # written twice. `git http-backend` emits only distinct names today
            # (`Expires`, `Pragma`, `Cache-Control`), so nothing collapses right
            # now — which is exactly the state the request path was in before
            # `captured_headers` was fixed, and the reason this side matches it
            # rather than waiting for a producer to prove the need.
            for name, value in response_headers or ():
                if name.lower() in ("content-length", "transfer-encoding", "connection"):
                    continue
                handler.send_header(name, value)
            for name, value in (extra_headers or {}).items():
                handler.send_header(name, value)
            if not (response_headers or extra_headers):
                handler.send_header("Content-Type", "text/plain")
            if close:
                # A refused body leaves the socket desynchronised, so the
                # connection ends here rather than framing the next request
                # with what was never read.
                handler.send_header("Connection", "close")
            handler.send_header("Content-Length", str(len(body)))
            handler.end_headers()
            if body:
                handler.wfile.write(body)
            if close:
                self._git_http_drain(handler)
        except OSError:
            # The peer vanished mid-response (the in-flight shutdown case).
            # The record above stands: what the bridge decided is evidence even
            # when nobody was left to read it.
            handler.close_connection = True

    @staticmethod
    def _git_http_drain(handler: _Handler) -> None:
        """Read what is left of a refused request, briefly, then let the
        handler close.

        Closing a socket with unread bytes still buffered sends an RST on
        Linux, and an RST discards the 400 the client has not read yet — the
        client then sees a connection error instead of the status the refusal
        tests assert.
        """
        handler.close_connection = True
        try:
            handler.connection.settimeout(_DRAIN_TIMEOUT_SECONDS)
            while handler.connection.recv(65536):
                pass
        except OSError:
            pass

    def _git_http_backend(
        self,
        handler: _Handler,
        *,
        project: str,
        method: str,
        path: str,
        query_string: str,
        service: str,
        body: bytes,
        framing: BodyFraming,
    ) -> None:
        """Run `git http-backend` as CGI for one request and answer with it."""
        env = git_environment(
            self.git_scratch_home,
            extra=cgi_environment(
                method=method,
                path_info=path,
                query_string=query_string,
                headers=captured_headers(handler.headers),
                project_root=self.git_project_root,
                content_length=len(body) if method == "POST" else None,
                remote_user=self.token_identity_login,
                remote_addr=handler.client_address[0],
            ),
        )
        completed = subprocess.run(
            ["git", "http-backend"],
            input=body,
            env=env,
            capture_output=True,
            timeout=_GIT_TIMEOUT_SECONDS,
            check=False,
        )
        status, response_headers, out = split_cgi_response(completed.stdout)
        self._git_http_reply(
            handler,
            project=project,
            method=method,
            path=path,
            query_string=query_string,
            service=service,
            status=status,
            body=out,
            framing=framing,
            body_bytes=len(body),
            cgi_env=env,
            # `http-backend` exits 0 whatever `Status:` it emitted, so its exit
            # code carries no signal at all and this is the only diagnosis a
            # failing request has.
            backend_stderr=completed.stderr.decode(errors="replace"),
            # `.items()` on an `HTTPMessage` yields one pair per header LINE,
            # duplicates included; a dict comprehension here would keep the last.
            response_headers=response_headers.items(),
        )

    def _git_http_write_hook_config(self) -> None:
        """Publish the current refusal / delay knobs where the hooks read them.

        Written per receive-pack rather than at project creation: every knob is
        mutated by a test after the server is already serving.
        """
        (self._git_http_state / _HOOK_CONFIG_NAME).write_text(
            json.dumps(
                {
                    "refusal": self.git_http_pre_receive_refusal,
                    "forbidden_options": sorted(self.git_http_forbidden_push_options),
                    "delay": self.git_http_merge_request_delay,
                }
            ),
            encoding="utf-8",
        )

    def _git_http_apply_concurrent_advance(self, project_path: str) -> None:
        """Land an armed racing writer's commit before the receive-pack ref
        advertisement, so the client sees a genuine non-fast-forward.

        Fires ONCE then clears — that is what proves C-043's retry converges
        rather than merely runs. `git_seed_files` re-imports the racing commit,
        which is the one place DX-5's two stores could otherwise drift.
        """
        prefix = f"{project_path}/"
        for key in [k for k in self.git_http_concurrent_advance if k.startswith(prefix)]:
            files = self.git_http_concurrent_advance.pop(key)
            self.git_seed_files(project_path, key[len(prefix) :], files)

    def _git_http_import_all_refs(self, project_path: str) -> None:
        repository = self.git_repository_path(project_path)
        listing = run_git(
            "-C",
            str(repository),
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            home=self.git_scratch_home,
        )
        for branch in listing.stdout.split():
            self.git_import_ref(project_path, branch)

    # ── setup ────────────────────────────────────────────────────────────

    def git_create_project(
        self,
        project_path: str,
        *,
        config: Mapping[str, str] = BARE_REPO_CONFIG,
        initial_branch: str = INITIAL_BRANCH,
    ) -> Path:
        """Create the bare repository for `<owner>/<repo>` and register it as a
        git route. Returns its path on disk.

        Registration is what makes the route predicate exact: an unregistered
        project is not a git path, whatever it looks like. Two projects on one
        host (S-040) is two calls.

        Also registers the project on the REST surfaces if it is not there yet,
        so the same `<owner>/<repo>` is addressable as a GitHub repo, a GitLab
        project and a git remote — the single-oracle property this fixture is
        built on.
        """
        existing = self.git_http_projects.get(project_path)
        if existing is not None:
            return existing
        repository = create_bare_repository(
            self.git_project_root,
            project_path,
            home=self.git_scratch_home,
            config=config,
            initial_branch=initial_branch,
        )
        self._git_http_install_hooks(repository)
        self.git_http_projects[project_path] = repository
        owner, _, _ = project_path.partition("/")
        with self.lock:
            self.repos.setdefault(
                project_path,
                {"full_name": project_path, "owner": owner, "parent": None},
            )
            # A GitLab id is assigned here rather than on first REST read, so
            # `gl_project_id` answers the same number the git route was
            # registered under.
            self._gl_project_id_locked(project_path)
        return repository

    def _git_http_install_hooks(self, repository: Path) -> None:
        """Write the `pre-receive` / `post-receive` pair into a bare repository.

        Installed at the repository's default `hooks/` path, which is what makes
        `::test_post_receive_hook_invocation_counter_is_nonzero`'s negative
        control — a `core.hooksPath` pointed at an empty directory — silence
        them.
        """
        state = self._git_http_state
        hooks = repository / "hooks"
        hooks.mkdir(parents=True, exist_ok=True)
        for hook in ("pre-receive", "post-receive"):
            script = hooks / hook
            script.write_text(
                _HOOK_SOURCE.format(
                    python=sys.executable,
                    config=repr(str(state / _HOOK_CONFIG_NAME)),
                    log=repr(str(state / _HOOK_LOG_NAME)),
                    root=repr(str(self.git_project_root)),
                    hook=repr(hook),
                ),
                encoding="utf-8",
            )
            script.chmod(0o755)

    def git_repository_path(self, project_path: str) -> Path:
        """The bare repository's path on disk, for a test that drives raw `git`
        against it (arming `core.hooksPath`, dropping a config row)."""
        repository = self.git_http_projects.get(project_path)
        if repository is None:
            raise GitFixtureError(
                f"{project_path} is not a git-transport project: create it with "
                "git_create_project first"
            )
        return repository

    def git_url(self, project_path: str, *, dot_git: bool = True) -> str:
        """The clone/push URL for a project on this server.

        `dot_git=False` yields the suffix-less spelling; `PATH_INFO` tolerates
        both, and a test that wants to prove that drives both.
        """
        suffix = ".git" if dot_git else ""
        return f"{self.base_url}/{project_path}{suffix}"

    def git_seed_files(
        self,
        project_path: str,
        branch: str,
        files: Mapping[str, bytes],
        *,
        base_branch: str = INITIAL_BRANCH,
    ) -> str:
        """Commit `files` onto `branch` with real `git`, then import the real sha.

        The git-transport counterpart of `FakeForge.seed_files`, and the one
        every WP-17 test needs: each of them requires a committed
        `p/<ns>/<pkg>.json` **in the bare repository** — the thing ocx clones —
        and `seed_files`/`seed_root` write only the in-memory graph. On a
        git-registered project those now refuse outright (`_seed_files_locked`),
        so this is the sole seeding route there.

        Creates `branch` off `base_branch` when it does not exist, and
        `base_branch` with an initial commit when *it* does not. Returns the new
        head's real sha. `git_seed_branch` is built on this.
        """
        repository = self.git_repository_path(project_path)
        parent = self._git_rev_parse(repository, branch)
        if parent is None:
            parent = self._git_rev_parse(repository, base_branch)
            if parent is None and base_branch != branch:
                parent = self._git_commit_into_bare(
                    repository, base_branch, {}, parent=None, message="initial commit"
                )
                self.git_import_ref(project_path, base_branch)
        sha = self._git_commit_into_bare(
            repository, branch, files, parent=parent, message=f"seed {branch}"
        )
        self.git_import_ref(project_path, branch)
        return sha

    def _git_rev_parse(self, repository: Path, branch: str) -> str | None:
        """A branch head as the BARE REPOSITORY reports it, or None."""
        completed = run_git(
            "-C",
            str(repository),
            "rev-parse",
            "--verify",
            "--quiet",
            f"refs/heads/{branch}",
            home=self.git_scratch_home,
            check=False,
        )
        head = completed.stdout.strip()
        return head or None

    def _git_commit_into_bare(
        self,
        repository: Path,
        branch: str,
        files: Mapping[str, bytes],
        *,
        parent: str | None,
        message: str,
    ) -> str:
        """Commit `files` onto `branch` in a BARE repository, returning the sha.

        The index-file chain, the same shape C-038 names: `hash-object -w` per
        file, `read-tree` the parent into a scratch `GIT_INDEX_FILE`,
        `update-index --add --cacheinfo`, `write-tree`, then a commit object.
        A bare repository has no worktree, so there is no other route.
        """
        home = self.git_scratch_home
        index = self._git_http_state / f"index-{os.getpid()}-{time.monotonic_ns()}"
        environment = {"GIT_INDEX_FILE": str(index)}
        try:
            if parent is not None:
                run_git(
                    "-C", str(repository), "read-tree", parent,
                    home=home, extra_env=environment,
                )
            for path, content in files.items():
                blob = subprocess.run(
                    ["git", "-C", str(repository), "hash-object", "-w", "--stdin"],
                    input=content,
                    env=git_environment(home),
                    capture_output=True,
                    timeout=_GIT_TIMEOUT_SECONDS,
                    check=True,
                ).stdout.decode().strip()
                run_git(
                    "-C", str(repository), "update-index", "--add",
                    "--cacheinfo", f"100644,{blob},{path}",
                    home=home, extra_env=environment,
                )
            tree = run_git(
                "-C", str(repository), "write-tree", home=home, extra_env=environment
            ).stdout.strip()
        finally:
            index.unlink(missing_ok=True)
        sha = self._git_commit_tree(repository, tree, parents=[parent] if parent else [], message=message)
        run_git(
            "-C", str(repository), "update-ref", f"refs/heads/{branch}", sha, home=home
        )
        return sha

    def _git_commit_tree(
        self, repository: Path, tree: str, *, parents: Sequence[str], message: str
    ) -> str:
        """A commit object over `tree`, with a FIXED identity.

        The identity is the fixture's own and is deliberately not ocx's, so
        C-045's "ocx set the commit identity" assertion cannot agree with a
        commit this fixture authored.
        """
        arguments = ["-C", str(repository), "commit-tree", tree]
        for parent in parents:
            arguments += ["-p", parent]
        arguments += ["-m", message]
        return run_git(
            *arguments,
            home=self.git_scratch_home,
            extra_env={
                "GIT_AUTHOR_NAME": "OCX Fixture",
                "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
                "GIT_COMMITTER_NAME": "OCX Fixture",
                "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
            },
        ).stdout.strip()

    def git_seed_branch(
        self,
        project_path: str,
        branch: str,
        state: BranchSeedState,
        *,
        base_branch: str = INITIAL_BRANCH,
        files: Mapping[str, bytes] | None = None,
    ) -> str | None:
        """Bring `branch` to one of C-051's six states relative to `base_branch`,
        git-first (DX-5).

        Built on `git_seed_files`, so the commits are real and the imported shas
        are real: a REST read and a `--force-with-lease` answer the same one.

        `files` supplies the differing content for `AHEAD_DIFFERING` and
        `DIVERGED`; it is ignored for `AHEAD_IDENTICAL`, whose whole point is a
        distinct commit carrying the base's tree unchanged.

        Returns the branch head's real sha, or `None` for `ABSENT`.
        """
        repository = self.git_repository_path(project_path)
        base = self._git_rev_parse(repository, base_branch)
        if base is None:
            raise GitFixtureError(
                f"{project_path}: {base_branch} does not exist, so no state can be "
                "seeded relative to it"
            )
        differing = dict(files or {".fixture-branch": b"seeded by git_seed_branch\n"})
        advance = {".fixture-base-advance": b"a second writer landed here\n"}

        match state:
            case BranchSeedState.ABSENT:
                run_git(
                    "-C", str(repository), "update-ref", "-d",
                    f"refs/heads/{branch}",
                    home=self.git_scratch_home, check=False,
                )
                with self.lock:
                    self.refs.get(f"{project_path}", {}).pop(branch, None)
                return None
            case BranchSeedState.IDENTICAL:
                run_git(
                    "-C", str(repository), "update-ref", f"refs/heads/{branch}", base,
                    home=self.git_scratch_home,
                )
                self.git_import_ref(project_path, branch)
                return base
            case BranchSeedState.AHEAD_IDENTICAL:
                # A DISTINCT commit carrying the base's tree byte for byte —
                # the state that separates "nothing to do" from "nothing to
                # commit but a request to ensure". Collapsing it into IDENTICAL
                # erases exactly that case.
                tree = run_git(
                    "-C", str(repository), "rev-parse", f"{base}^{{tree}}",
                    home=self.git_scratch_home,
                ).stdout.strip()
                sha = self._git_commit_tree(
                    repository, tree, parents=[base], message="ahead, identical tree"
                )
                run_git(
                    "-C", str(repository), "update-ref", f"refs/heads/{branch}", sha,
                    home=self.git_scratch_home,
                )
                self.git_import_ref(project_path, branch)
                return sha
            case BranchSeedState.AHEAD_DIFFERING:
                return self.git_seed_files(
                    project_path, branch, differing, base_branch=base_branch
                )
            case BranchSeedState.BEHIND:
                run_git(
                    "-C", str(repository), "update-ref", f"refs/heads/{branch}", base,
                    home=self.git_scratch_home,
                )
                self.git_import_ref(project_path, branch)
                self.git_seed_files(project_path, base_branch, advance)
                return base
            case BranchSeedState.DIVERGED:
                sha = self.git_seed_files(
                    project_path, branch, differing, base_branch=base_branch
                )
                self.git_seed_files(project_path, base_branch, advance)
                return sha
        raise GitFixtureError(f"unhandled branch state {state!r}")

    def git_import_ref(self, project_path: str, branch: str) -> str | None:
        """Mirror a bare-repo ref and its whole tree into the in-memory graph,
        keyed by the **real** sha.

        Called by `git_seed_files` and again after every accepted push and every
        `git_http_concurrent_advance`, so the REST surfaces answer what actually
        exists on disk.

        Registers each commit through **`FakeForge._record_commit_locked`**,
        never by writing `commits` / `file_last_commit` directly. That method
        owns the per-path provenance rule GitLab's compare-and-swap reads
        (`fake_gitlab.py::gl_post_commit`), and its own comment says getting it
        wrong makes the CAS either never fire or always fire — so it gets one
        writer, not a restatement here.

        Returns the imported sha, or `None` when the ref does not exist.
        """
        repository = self.git_repository_path(project_path)
        head = self._git_rev_parse(repository, branch)
        if head is None:
            return None
        home = self.git_scratch_home
        listing = run_git(
            "-C", str(repository), "log", "--first-parent", "--reverse",
            "--format=%H %P", head, home=home,
        ).stdout
        owner, _, _ = project_path.partition("/")
        for line in listing.splitlines():
            fields = line.split()
            if not fields:
                continue
            commit, parents = fields[0], fields[1:]
            with self.lock:
                known = commit in self.commits
            if known:
                continue
            files = self._git_read_tree(repository, commit)
            with self.lock:
                flat = {
                    path: self._store_blob_locked(content)
                    for path, content in files.items()
                }
                tree_sha = self._store_tree_locked(flat)
                # Registered through `_record_commit_locked`, never by writing
                # `commits` / `file_last_commit` here: that method owns the
                # per-path provenance rule GitLab's compare-and-swap reads, and
                # a restatement is how the CAS ends up never firing or always
                # firing.
                self._record_commit_locked(
                    commit, tree_sha, parents[0] if parents else None
                )
        with self.lock:
            self.refs.setdefault(project_path, {})[branch] = head
            self.repos.setdefault(
                project_path,
                {"full_name": project_path, "owner": owner, "parent": None},
            )
        return head

    def _git_read_tree(self, repository: Path, commit: str) -> dict[str, bytes]:
        """A commit's whole tree as `{path: content}`, read from the bare repo."""
        listing = _run_git_bytes(
            "-C", str(repository), "ls-tree", "-r", "-z", commit,
            home=self.git_scratch_home,
        )
        files: dict[str, bytes] = {}
        for entry in listing.split(b"\0"):
            if not entry:
                continue
            # `<mode> SP <type> SP <sha> TAB <path>`; `-z` means the path is
            # raw rather than quoted, so a name with a space survives.
            meta, _, path = entry.partition(b"\t")
            fields = meta.split()
            if len(fields) < 3 or fields[1] != b"blob":
                continue
            files[path.decode()] = _run_git_bytes(
                "-C", str(repository), "cat-file", "blob", fields[2].decode(),
                home=self.git_scratch_home,
            )
        return files

    def git_head(self, project_path: str, branch: str) -> str | None:
        """The branch head as the **bare repository** reports it.

        Deliberately alongside `FakeForge.branch_head`, which reads the
        in-memory graph. The two agreeing is DX-5's whole claim, so it is
        asserted rather than assumed — a single accessor could not express the
        question.
        """
        return self._git_rev_parse(self.git_repository_path(project_path), branch)

    # ── observation ──────────────────────────────────────────────────────

    def git_pushes(self, project_path: str | None = None) -> list[PushRecord]:
        """Every receive-hook invocation recorded so far, oldest first.

        Re-read from disk on each call: the hooks are subprocesses that append
        while the test is blocked in `ocx`.

        **Must not acquire `self.lock`.** `git_promote_ready_merge_requests_locked`
        calls it with the lock already held (from `gl_get_merge_requests`), so an
        implementation that takes it deadlocks every merge-request poll. Nothing
        it reads is in-memory server state — the log is a file — so there is
        nothing for the lock to protect here.
        """
        root = self._git_http_root
        if root is None:
            return []
        log = root / "state" / _HOOK_LOG_NAME
        if not log.exists():
            return []
        pushes: list[PushRecord] = []
        for line in log.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            payload = json.loads(line)
            if project_path is not None and payload["project"] != project_path:
                continue
            pushes.append(
                PushRecord(
                    hook=payload["hook"],
                    invocation=payload["invocation"],
                    project=payload["project"],
                    options=tuple(payload["options"]),
                    option_count_present=payload["option_count_present"],
                    option_count=payload["option_count"],
                    updates=tuple(
                        PushedRef(old=ref["old"], new=ref["new"], ref=ref["ref"])
                        for ref in payload["updates"]
                    ),
                    ready_at=payload["ready_at"],
                )
            )
        return pushes

    def git_promote_ready_merge_requests_locked(self) -> None:
        """Materialise a merge request for every push whose `ready_at` has
        passed. Called from `gl_get_merge_requests` with `self.lock` held.

        This is the lazy readiness gate: the `post-receive` hook writes
        `{ready_at, options, ref}` and exits immediately, and readiness is
        computed at **read** time. A sleeping hook would hold the CGI child and
        the HTTP response open for the whole delay, wedging the push it is meant
        to complete.

        The record it writes has the same shape `gl_post_merge_request` writes,
        so the client cannot tell an asynchronously created request from a
        synchronously created one — which is the production shape D-T4
        describes.

        The empty-registry fast path is implemented, not stubbed:
        `gl_get_merge_requests` is on `test_announce_gitlab.py`'s hot path.
        """
        if not self.git_http_projects:
            return
        now = time.time()
        for push in self.git_pushes():
            if push.invocation in self._git_http_promoted:
                continue
            if push.ready_at is None or push.ready_at > now:
                continue
            options = dict(
                option.split("=", 1) if "=" in option else (option, "")
                for option in push.options
            )
            if "merge_request.create" not in options:
                # A push that asked for no merge request never gets one — the
                # same rule the real forge applies to its push options.
                self._git_http_promoted.add(push.invocation)
                continue
            branches = [
                update.ref.removeprefix("refs/heads/")
                for update in push.updates
                if update.ref.startswith("refs/heads/") and set(update.new) != {"0"}
            ]
            if not branches:
                self._git_http_promoted.add(push.invocation)
                continue
            branch = branches[0]
            key = (push.project, push.project, branch)
            self._git_http_promoted.add(push.invocation)
            if key in self.gitlab_merge_requests:
                continue
            self._gitlab_mr_counter += 1
            number = self._gitlab_mr_counter
            self.gitlab_merge_requests[key] = {
                "id": 90000 + number,
                "iid": number,
                "web_url": f"{self.base_url}/{push.project}/-/merge_requests/{number}",
                "state": "opened",
                "target_branch": options.get("merge_request.target", INITIAL_BRANCH),
                "source_branch": branch,
                # A push carries its own branch onto the project it pushed to, so
                # source and target are the same project. The client reads these
                # two ids instead of resolving a numeric project id of its own —
                # `GET /projects/:id` is not job-token-readable (ocx#429) — and
                # compares them to reject a stranger's request on the same
                # deterministic branch name.
                "source_project_id": self._gl_project_id_locked(push.project),
                "target_project_id": self._gl_project_id_locked(push.project),
            }


__all__ = [
    "BARE_REPO_CONFIG",
    "BODY_READ_TIMEOUT_SECONDS",
    "FIXTURE_HOME_USER_EMAIL",
    "FIXTURE_HOME_USER_NAME",
    "GIT_PROTOCOL_HEADER",
    "INITIAL_BRANCH",
    "MAX_CHUNKED_BODY_BYTES",
    "OBSERVED_REFUSAL_STDERR",
    "BodyFraming",
    "BranchSeedState",
    "ChunkedBodyError",
    "ChunkedBodyRejection",
    "CredentialHelperCall",
    "FixtureHome",
    "GitFixtureError",
    "GitHttpRoutes",
    "GitRequest",
    "PushRecord",
    "PushedRef",
    "RefusalShape",
    "build_fixture_home",
    "captured_headers",
    "cgi_environment",
    "create_bare_repository",
    "git_environment",
    "read_chunked_body",
    "run_git",
    "split_cgi_response",
]
