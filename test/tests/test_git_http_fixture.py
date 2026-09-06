# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Executable specification for the git-over-HTTP fixture and the `git` shim.

WP-4 of `.claude/artifacts/plan_index_claim_command.md`. Written in the Specify
phase against the plan's contracts and the declared surface of
`git_http_fixture.py` / `git_shim.py` — never against an implementation, which
does not exist yet.

**This module never starts the OCI registry.** Like
`test_fake_forge_mergeability.py`, it consumes the `fake_forge` fixture (a
loopback `http.server` on an ephemeral port) and `tmp_path`, and nothing else.
The subject under test is the fixture itself: every WP-16 and WP-17 assertion
about ocx is measured through these routes, so a fixture that lies makes roughly
twenty downstream security assertions unfalsifiable.

Three shapes recur, each because `subsystem-tests.md` § Unfalsifiable Greens
names its failure mode:

* **Capture windows are sliced, not searched.** Every test records
  `len(fake_forge.git_http_requests)` before it acts and asserts over the slice
  it caused, rather than picking an entry with `[-1]` off a log another request
  may have appended to.
* **Negatives are guarded by a positive.** "No `Authorization` reached the
  sibling" and "the REST log did not grow" are asserted only after the
  corresponding collection is proved non-empty.
* **Every socket carries a timeout.** There is no `pytest-timeout` in
  `test/pyproject.toml`, so a wedged fixture would hang the whole suite rather
  than fail one test.
"""

from __future__ import annotations

import http.client
import io
import json
import os
import shutil
import socket
import stat
import subprocess
import sys
import time
import urllib.parse
from pathlib import Path
from typing import TYPE_CHECKING

import pytest
from git_http_fixture import (
    BARE_REPO_CONFIG,
    FETCH_REFUSAL_NEEDLES,
    GIT_PROTOCOL_HEADER,
    INITIAL_BRANCH,
    OBSERVED_REFUSAL_STDERR,
    BodyFraming,
    BranchSeedState,
    ChunkedBodyError,
    ChunkedBodyRejection,
    GitFixtureError,
    RefusalShape,
    build_fixture_home,
    git_environment,
    read_chunked_body,
    run_git,
    split_cgi_response,
)
from git_shim import (
    DENIED_SHIM_ROOTS,
    RECORD_NAME,
    SHIM_NAME,
    VERSION_OVERRIDES,
    GitShimVariant,
    ShimPlacementError,
    install_git_shim,
    render_shim_source,
    resolve_real_git,
)

if TYPE_CHECKING:  # pragma: no cover - typing only
    from fake_forge import FakeForge
    from git_http_fixture import FixtureHome, GitRequest

# ── constants ────────────────────────────────────────────────────────────────

#: The index project every test that needs only one project uses.
PROJECT = "ocx-sh/index"

#: A second project on the SAME host. S-040's question ("is the credential sent
#: to a sibling?") is only a real question when the sibling shares the origin,
#: which is why the fixture serves both from one port.
SIBLING = "ocx-sh/other"

#: The claim root a seeded project carries. Content, not shape, matters here.
ROOT_PATH = "p/acme/widget.json"
ROOT_BYTES = b'{"name":"acme/widget","tags":{}}\n'

#: The branch a claim pushes to. Deliberately not `INITIAL_BRANCH`: C-038's
#: first-claim path is a ref CREATE, and a push onto the existing default branch
#: could never show the `0`*40 old-value that proves it.
CLAIM_BRANCH = "ocx-claim-acme-widget"

#: The four push-option keys C-039 names, and only those. Held as the sorted key
#: SET rather than a count: substituting
#: `merge_request.merge_when_pipeline_succeeds` for `.description` keeps the
#: count at four while auto-merging the claim and defeating G-04.
CLAIM_PUSH_OPTIONS = (
    "merge_request.create",
    "merge_request.target=main",
    "merge_request.title=Claim acme/widget",
    "merge_request.description=Claim opened by the ocx test fixture.",
)
FOUR_OPTION_KEYS = frozenset(
    {
        "merge_request.create",
        "merge_request.target",
        "merge_request.title",
        "merge_request.description",
    }
)

#: The option key C-039 forbids by name. The fixture's `pre-receive` hook is
#: armed with it so the refusal is SERVER-side, not a re-read of what the client
#: happened to send.
FORBIDDEN_OPTION_KEY = "merge_request.merge_when_pipeline_succeeds"

#: An obviously fake credential. It is base64 of `ocx:fake-token` and is the
#: only credential-shaped string in this module; nothing here may ever carry a
#: real one.
FAKE_AUTHORIZATION = "Basic b2N4OmZha2UtdG9rZW4="

#: What git's own stderr says when the receive-pack path answers 401 and no
#: credential source can answer the challenge. Measured against git 2.54.0, not
#: assumed: with `GIT_TERMINAL_PROMPT=0` (`git_environment`) the prompt git would
#: fall back to is disabled, so the run dies naming the credential it could not
#: obtain rather than reporting a bare HTTP status.
AUTH_FAILURE_NEEDLE = "terminal prompts disabled"

#: Every socket in this module carries a timeout, because a wedged fixture would
#: otherwise hang the entire suite (`test/pyproject.toml` has no pytest-timeout).
HTTP_TIMEOUT_SECONDS = 15.0

#: How long a readiness poll may run before the test fails rather than hangs.
POLL_DEADLINE_SECONDS = 15.0

#: The `post-receive` delay `::test_post_receive_records_after_delay` uses. Large
#: enough that "the push returned before the delay elapsed" is a real assertion
#: about a non-sleeping hook rather than a race with a fast local push.
LONG_READY_DELAY_SECONDS = 5.0

#: A ceiling low enough that C-074's `OVERSIZED` refusal is reachable without
#: sending megabytes over loopback.
SMALL_BODY_CEILING_BYTES = 32

#: `git_http_body_read_timeout_seconds` for the abandonment test. The 30s
#: default is longer than any bound a test may block for, which is exactly why
#: the timeout is a per-server knob: an unobservable guard is one nobody notices
#: losing.
SHORT_BODY_READ_TIMEOUT_SECONDS = 0.5

#: How long after the window closes the abandonment must have happened. Well
#: above `SHORT_BODY_READ_TIMEOUT_SECONDS` so a loaded machine does not flake,
#: and well below the 30s default so restoring the default reds the assertion.
ABANDON_BOUND_SECONDS = 5.0


# ── helpers ──────────────────────────────────────────────────────────────────


def _home(root: Path, name: str = "home", **kwargs: object) -> FixtureHome:
    """A scratch `HOME` under `root`, one per arm so two arms never share state."""
    directory = root / name
    directory.mkdir(parents=True, exist_ok=True)
    return build_fixture_home(directory, **kwargs)


def _seeded(fake_forge: FakeForge, project: str, **kwargs: object) -> str:
    """Register `project` as a git route and commit the claim root onto `main`."""
    fake_forge.git_create_project(project, **kwargs)
    return fake_forge.git_seed_files(project, INITIAL_BRANCH, {ROOT_PATH: ROOT_BYTES})


def _push_credential(fake_forge: FakeForge, project: str) -> tuple[str, str]:
    """The `-c` pair that authorizes a push to `project`.

    The bridge answers 401 to a receive-pack request carrying no usable HTTP
    Basic credential, so every push here has to spell one — a clone does not, and
    deliberately still does not, because the read half stays anonymous until
    `git_http_credential` closes it.

    Scoped to the project's own URL rather than set bare, so a push that is
    redirected to a sibling reaches it the way ocx's own scoped credential would:
    without the header. `FAKE_AUTHORIZATION` is the only credential-shaped string
    in this module and it authenticates nothing — the fixture accepts any
    well-formed pair unless `git_http_credential` narrows it.
    """
    return (
        "-c",
        f"http.{fake_forge.git_url(project)}.extraHeader=Authorization: {FAKE_AUTHORIZATION}",
    )


def _clone(
    fake_forge: FakeForge,
    project: str,
    home: FixtureHome,
    dest: Path,
    *extra: str,
    url: str | None = None,
) -> Path:
    run_git(
        "clone",
        *extra,
        url if url is not None else fake_forge.git_url(project),
        str(dest),
        home=home.path,
    )
    return dest


def _commit(work: Path, home: FixtureHome, files: dict[str, bytes], message: str) -> str:
    for name, content in files.items():
        target = work / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)
    run_git("-C", str(work), "add", "-A", home=home.path)
    run_git("-C", str(work), "commit", "-m", message, home=home.path)
    return run_git("-C", str(work), "rev-parse", "HEAD", home=home.path).stdout.strip()


def _push(
    fake_forge: FakeForge,
    project: str,
    work: Path,
    home: FixtureHome,
    refspec: str,
    *extra: str,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    return run_git(
        "-C",
        str(work),
        *_push_credential(fake_forge, project),
        "push",
        *extra,
        fake_forge.git_url(project),
        refspec,
        home=home.path,
        check=check,
    )


def _claim_push_ready(
    fake_forge: FakeForge, tmp_path: Path, project: str, *, name: str = "claim"
) -> tuple[FixtureHome, Path]:
    """A seeded project plus a clone carrying one unpushed claim commit."""
    _seeded(fake_forge, project)
    home = _home(tmp_path, f"home-{name}")
    work = tmp_path / f"work-{name}"
    _clone(fake_forge, project, home, work)
    _commit(work, home, {ROOT_PATH: ROOT_BYTES + b'{"claimed":true}\n'}, "claim acme/widget")
    return home, work


def _post_receive(fake_forge: FakeForge, project: str) -> list:
    """Only the `post-receive` records for `project`, oldest first.

    Filtered by hook rather than taken wholesale: an accepted push runs
    `pre-receive` too, and an assertion over the union could not tell "the
    post-receive hook ran" from "some hook ran".
    """
    return [record for record in fake_forge.git_pushes(project) if record.hook == "post-receive"]


def _option_keys(record) -> frozenset[str]:
    return frozenset(option.split("=", 1)[0] for option in record.options)


def _header(request: GitRequest, name: str) -> str | None:
    """The single value sent under `name`, or None when it was not sent.

    Reads through `GitRequest.header_values`, which is multi-valued, and FAILS
    on a second value rather than picking one. `http.extraHeader` is
    multi-valued in git, so two `Authorization` headers on one request is a
    reachable state and a silent pick is how the regression stays unreported —
    a test that wants both asks `header_values` for them.
    """
    values = request.header_values(name)
    assert len(values) <= 1, (
        f"{name} arrived {len(values)} times on {request.method} {request.path}: "
        f"{values!r}. Assert with `header_values` — a single-value read here "
        "would silently drop one of them"
    )
    return values[0] if values else None


def _host_port(fake_forge: FakeForge) -> tuple[str, int]:
    parts = urllib.parse.urlsplit(fake_forge.base_url)
    assert parts.hostname is not None
    assert parts.port is not None
    return parts.hostname, parts.port


def _http_get(
    fake_forge: FakeForge, path: str, headers: dict[str, str] | None = None
) -> tuple[int, bytes]:
    host, port = _host_port(fake_forge)
    conn = http.client.HTTPConnection(host, port, timeout=HTTP_TIMEOUT_SECONDS)
    try:
        conn.request("GET", path, headers=headers or {})
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


def _http_post(
    fake_forge: FakeForge, path: str, body: bytes, content_type: str, *, authorized: bool = False
) -> tuple[int, bytes]:
    """POST `body`, optionally carrying the fixture's HTTP Basic credential.

    `authorized` is opt-in rather than always-on: the receive-pack route answers
    401 without a credential, so a caller that wants the request to reach
    `http-backend` at all must ask for one, and a caller probing the refusal must
    be able to leave it off.
    """
    host, port = _host_port(fake_forge)
    conn = http.client.HTTPConnection(host, port, timeout=HTTP_TIMEOUT_SECONDS)
    headers = {"Content-Type": content_type}
    if authorized:
        headers["Authorization"] = FAKE_AUTHORIZATION
    try:
        conn.request("POST", path, body=body, headers=headers)
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


class _ChunkedReply:
    """What a hand-built chunked POST observed.

    `probe` is a `dup()` of the connection socket taken before the response is
    read, so "the server closed the connection" stays observable after
    `http.client` has closed its own reference.
    """

    def __init__(self, status: int, will_close: bool, probe: socket.socket) -> None:
        self.status = status
        self.will_close = will_close
        self.probe = probe


def _chunked_post(
    fake_forge: FakeForge,
    path: str,
    raw_body: bytes,
    *,
    half_close: bool = False,
) -> _ChunkedReply:
    """POST `raw_body` as the literal chunked framing, byte for byte.

    Real `git` emits neither a chunk extension nor a trailer section, so C-074's
    first two refusals are reachable only from a hand-built stream. The body is
    written verbatim — no encoder sits between the test and the bytes the
    decoder must reject.
    """
    host, port = _host_port(fake_forge)
    conn = http.client.HTTPConnection(host, port, timeout=HTTP_TIMEOUT_SECONDS)
    conn.connect()
    probe = conn.sock.dup()
    probe.settimeout(HTTP_TIMEOUT_SECONDS)
    try:
        conn.putrequest("POST", path)
        conn.putheader("Content-Type", "application/x-git-receive-pack-request")
        conn.putheader("Transfer-Encoding", "chunked")
        conn.endheaders()
        conn.send(raw_body)
        if half_close:
            conn.sock.shutdown(socket.SHUT_WR)
        response = conn.getresponse()
        status, will_close = response.status, response.will_close
        response.read()
        return _ChunkedReply(status, will_close, probe)
    finally:
        conn.close()


def _content_length_post(
    fake_forge: FakeForge, path: str, body: bytes, *, declared: str
) -> int:
    """POST `body` under a LITERAL `Content-Length: <declared>`, returning the status.

    `http.client.request` computes the header itself, so a declaration that
    disagrees with the body — oversized, negative, or not a number — is
    reachable only by writing the header by hand. `body` stays a few bytes so it
    fits the socket buffer: the server refuses before reading it and closes, and
    a large body would race that close with an EPIPE on this side.
    """
    host, port = _host_port(fake_forge)
    conn = http.client.HTTPConnection(host, port, timeout=HTTP_TIMEOUT_SECONDS)
    try:
        conn.putrequest("POST", path)
        conn.putheader("Content-Type", "application/x-git-receive-pack-request")
        conn.putheader("Content-Length", declared)
        conn.endheaders()
        conn.send(body)
        response = conn.getresponse()
        status = response.status
        response.read()
        return status
    finally:
        conn.close()


def _http_patch(
    fake_forge: FakeForge, path: str, body: dict
) -> tuple[int, bytes]:
    host, port = _host_port(fake_forge)
    conn = http.client.HTTPConnection(host, port, timeout=HTTP_TIMEOUT_SECONDS)
    try:
        conn.request(
            "PATCH",
            path,
            body=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


def _receive_pack_path(fake_forge: FakeForge, project: str) -> str:
    return urllib.parse.urlsplit(fake_forge.git_url(project)).path + "/git-receive-pack"


def _refusal_stderr(fake_forge: FakeForge, tmp_path: Path, shape: RefusalShape) -> str:
    """Drive one server-side refusal and return the client's verbatim stderr.

    Each shape gets its own project so the six arms cannot interfere: the two
    403 knobs and the ref-advance knob are per-server, and a shared project would
    make the order of the six a hidden input.

    The per-project split is not enough on its own. `git_http_pre_receive_refusal`
    and `git_http_forbidden_push_options` are per-SERVER and neither is one-shot,
    so after the `NOT_ALLOWED_TO_PUSH` arm the refusal stays armed for shapes
    3-6. It is inert there only incidentally — those shapes fail before the hook
    runs or are refused client-side — which makes the enum's declaration order a
    hidden input: reorder `RefusalShape` and this silently measures something
    else. Both are therefore reset here, at the top of every arm.
    """
    fake_forge.git_http_pre_receive_refusal = None
    fake_forge.git_http_forbidden_push_options = set()

    project = f"ocx-sh/refusal-{shape.value}"
    home, work = _claim_push_ready(fake_forge, tmp_path, project, name=f"refusal-{shape.value}")
    base = fake_forge.git_head(project, INITIAL_BRANCH)
    assert base is not None, "the seeded base must exist before a refusal can be measured"

    extra: list[str] = []
    match shape:
        case RefusalShape.PRE_RECEIVE_DECLINED:
            fake_forge.git_http_pre_receive_refusal = "pre-receive hook refused this update"
        case RefusalShape.NOT_ALLOWED_TO_PUSH:
            fake_forge.git_http_pre_receive_refusal = (
                "You are not allowed to push code to this project."
            )
        case RefusalShape.FORBIDDEN_INFO_REFS:
            fake_forge.git_http_forbid_info_refs = True
        case RefusalShape.FORBIDDEN_RECEIVE_PACK:
            fake_forge.git_http_forbid_receive_pack = True
        case RefusalShape.NON_FAST_FORWARD:
            fake_forge.git_http_concurrent_advance[f"{project}/{INITIAL_BRANCH}"] = {
                "race.txt": b"a second writer landed here\n"
            }
        case RefusalShape.STALE_LEASE:
            fake_forge.git_http_concurrent_advance[f"{project}/{INITIAL_BRANCH}"] = {
                "race.txt": b"a second writer landed here\n"
            }
            extra = [f"--force-with-lease={INITIAL_BRANCH}:{base}"]

    result = _push(
        fake_forge,
        project,
        work,
        home,
        f"HEAD:refs/heads/{INITIAL_BRANCH}",
        *extra,
        check=False,
    )
    assert result.returncode != 0, (
        f"{shape.value} was ACCEPTED; a refusal shape that succeeds records no stderr "
        "and would publish an empty phrase to WP-12"
    )
    return result.stderr


# ── clone, push, hooks ───────────────────────────────────────────────────────


def test_clone_over_http(fake_forge: FakeForge, tmp_path: Path) -> None:
    """`git clone` over the fixture returns the committed bytes, under both URL
    spellings.

    The floor of the whole transport: without it every later assertion measures a
    server that never served. Both spellings are driven because `git_url`'s
    `dot_git=False` promises `PATH_INFO` tolerates the suffix-less form, and a
    promise nothing exercises is not a contract.
    """
    _seeded(fake_forge, PROJECT)
    home = _home(tmp_path)

    for label, dot_git in (("with-suffix", True), ("without-suffix", False)):
        before = len(fake_forge.git_http_requests)
        dest = tmp_path / f"clone-{label}"
        _clone(fake_forge, PROJECT, home, dest, url=fake_forge.git_url(PROJECT, dot_git=dot_git))

        assert (dest / ROOT_PATH).read_bytes() == ROOT_BYTES
        assert (
            run_git("-C", str(dest), "rev-parse", "HEAD", home=home.path).stdout.strip()
            == fake_forge.git_head(PROJECT, INITIAL_BRANCH)
        )

        served = fake_forge.git_http_requests[before:]
        assert served, f"the {label} clone reached no git route at all"
        assert all(request.project == PROJECT for request in served)
        assert any(
            request.method == "GET" and "service=git-upload-pack" in request.query
            for request in served
        ), "no smart-HTTP advertisement was served; the dumb path answered instead"


def test_push_delivers_exactly_the_four_option_keys(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The receive hook observes exactly C-039's four keys, with their values
    intact, and the create-shaped update that proves the first-claim direction.

    Asserted as the sorted key SET, not a count: substituting
    `merge_request.merge_when_pipeline_succeeds` for `.description` holds the
    count at four while auto-merging the claim.

    The `updates` assertion rides here because `PushedRef` has no named test of
    its own and `old == "0" * 40` is visible nowhere else on the wire.
    """
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT)

    options: list[str] = []
    for option in CLAIM_PUSH_OPTIONS:
        options += ["-o", option]
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}", *options)

    records = _post_receive(fake_forge, PROJECT)
    assert len(records) == 1, f"expected one post-receive record, got {records!r}"
    record = records[0]

    assert _option_keys(record) == FOUR_OPTION_KEYS
    assert list(record.options) == list(CLAIM_PUSH_OPTIONS), (
        "the hook must record the raw option values in order: a set-only capture "
        "cannot show a value mangled in transit"
    )
    # No assertion on `option_count_present`: git 2.54.0 always exports the
    # variable (DX-19), so `is True` has no reachable red state. The count is
    # the discriminating read.
    assert record.option_count == "4"

    assert len(record.updates) == 1
    update = record.updates[0]
    assert update.ref == f"refs/heads/{CLAIM_BRANCH}"
    assert update.old == "0" * 40, "a first claim must arrive as a ref CREATE"
    assert update.new == fake_forge.git_head(PROJECT, CLAIM_BRANCH)


def test_merge_when_pipeline_succeeds_is_rejected_by_the_hook(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """A forbidden option key is refused SERVER-side, and the accepted case
    proves the refusal is conditional.

    Without the accepted arm this degrades into re-reading `PushRecord.options`,
    which is already `::test_push_delivers_exactly_the_four_option_keys` — one
    assertion counted twice rather than the server-side refusal C-039 names.
    """
    fake_forge.git_http_forbidden_push_options = {FORBIDDEN_OPTION_KEY}
    fake_forge.git_http_pre_receive_refusal = "this option key is refused by policy"
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT)

    allowed: list[str] = []
    for option in CLAIM_PUSH_OPTIONS:
        allowed += ["-o", option]
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}", *allowed)
    assert fake_forge.git_head(PROJECT, CLAIM_BRANCH) is not None, (
        "the allowed arm must be accepted, or the refusal below proves only that "
        "the hook declines every push"
    )

    forbidden = [*allowed, "-o", f"{FORBIDDEN_OPTION_KEY}=true"]
    with pytest.raises(GitFixtureError) as excinfo:
        _push(fake_forge, PROJECT, work, home, "HEAD:refs/heads/auto-merged", *forbidden)

    assert "this option key is refused by policy" in str(excinfo.value), (
        "check=True must raise carrying the child's stderr; an error that drops it "
        "makes every failed setup step invisible"
    )
    assert fake_forge.git_head(PROJECT, "auto-merged") is None


def test_post_receive_records_after_delay(fake_forge: FakeForge, tmp_path: Path) -> None:
    """The hook stamps `ready_at` and exits; it never sleeps.

    A sleeping hook would hold the CGI child and the HTTP response open for the
    whole delay, wedging the very push it exists to complete — so the load-bearing
    assertion is that the push returned in well under the configured delay while
    still carrying the delayed readiness stamp.
    """
    fake_forge.git_http_merge_request_delay = LONG_READY_DELAY_SECONDS
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT)

    started = time.monotonic()
    wall_before = time.time()
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}")
    elapsed = time.monotonic() - started
    wall_after = time.time()

    assert elapsed < LONG_READY_DELAY_SECONDS, (
        f"the push took {elapsed:.2f}s against a {LONG_READY_DELAY_SECONDS}s delay: "
        "the hook slept instead of stamping a readiness instant"
    )

    records = _post_receive(fake_forge, PROJECT)
    assert len(records) == 1
    ready_at = records[0].ready_at
    assert ready_at is not None
    assert wall_before + LONG_READY_DELAY_SECONDS <= ready_at <= (
        wall_after + LONG_READY_DELAY_SECONDS
    ), "ready_at must be the hook's own instant plus the configured delay"


def test_post_receive_hook_invocation_counter_is_nonzero(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The hook actually ran — asserted before any assertion that filters its log.

    `HOME` reaches the child by design (C-033), so a developer's global
    `core.hooksPath`, or a `noexec` `TMPDIR`, silences every hook and every "the
    options parsed correctly" assertion then filters an empty log and passes. The
    second project is the in-test negative control: with `core.hooksPath` pointed
    at an empty directory the push still succeeds and the log IS empty, which is
    what proves the first project's non-empty log is a measurement.
    """
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT)
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}")
    _commit(work, home, {ROOT_PATH: ROOT_BYTES + b'{"second":true}\n'}, "second claim")
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}")

    records = _post_receive(fake_forge, PROJECT)
    assert len(records) == 2, f"the post-receive hook did not run twice: {records!r}"
    # `== 1`, not `>= 1`: the server is function-scoped and the log starts
    # empty, so 1 is exactly what the 1-based contract in `PushRecord` promises
    # and `>= 1` would accept a counter that started anywhere.
    assert records[0].invocation == 1
    assert records[1].invocation > records[0].invocation
    assert all(record.project == PROJECT for record in records)

    silenced = "ocx-sh/hookless"
    empty_hooks = tmp_path / "no-hooks"
    empty_hooks.mkdir()
    quiet_home, quiet_work = _claim_push_ready(
        fake_forge, tmp_path, silenced, name="hookless"
    )
    run_git(
        "-C",
        str(fake_forge.git_repository_path(silenced)),
        "config",
        "core.hooksPath",
        str(empty_hooks),
        home=quiet_home.path,
    )
    _push(fake_forge, silenced, quiet_work, quiet_home, f"HEAD:refs/heads/{CLAIM_BRANCH}")

    assert fake_forge.git_head(silenced, CLAIM_BRANCH) is not None, (
        "the control push must be ACCEPTED; a rejected push would leave the log "
        "empty for the wrong reason"
    )
    assert fake_forge.git_pushes(silenced) == [], (
        "a hookless repository still recorded a push: the log is not written by "
        "the hook, so the counter above proves nothing"
    )


def test_push_option_count_is_zero_when_no_options_sent(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """`GIT_PUSH_OPTION_COUNT` reads `"0"` with no options, `"4"` with four.

    The plan named this test for the ABSENT case, on the documented contract
    that git leaves the variable unset when the push-options phase was never
    negotiated. **git 2.54.0 does not do that** — it exports
    `GIT_PUSH_OPTION_COUNT=0` to receive hooks whether or not the client
    advertised `push-options`, with `receive.advertisePushOptions` either way,
    over both the local transport and smart HTTP. Verified with a plain
    `/bin/sh` hook outside this fixture, from a parent shell holding no
    `GIT_PUSH_*` variable at all.

    So the absent state is unreachable, and the only implementation that could
    satisfy an `is False` assertion is a hook that fabricates the absence — the
    exact defect the assertion existed to forbid. Proved by mutation: a hook
    written `environ.get("GIT_PUSH_OPTION_COUNT", "0")` turned the original
    assertion GREEN.

    What is reachable, and still discriminating, is the count and the values:
    dropping the options or miscounting them reds this. `option_count_present`
    stays on `PushRecord` as an honest report of what the environment held, but
    it is capture, **not a check** — no mutation can make it False on this git,
    so nothing here asserts on it and nothing later should.
    """
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT)

    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}")
    bare = _post_receive(fake_forge, PROJECT)
    assert len(bare) == 1
    assert bare[0].option_count == "0"
    assert list(bare[0].options) == []

    options: list[str] = []
    for option in CLAIM_PUSH_OPTIONS:
        options += ["-o", option]
    _commit(work, home, {ROOT_PATH: ROOT_BYTES + b'{"with-options":true}\n'}, "with options")
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}", *options)

    carried = _post_receive(fake_forge, PROJECT)
    assert len(carried) == 2
    assert carried[1].option_count == "4"
    assert list(carried[1].options) == list(CLAIM_PUSH_OPTIONS)


def test_rejection_hook_writes_literal_refusal_line(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The `pre-receive` hook writes the refusal line WITHOUT a `remote: ` prefix.

    `send-pack` adds that prefix itself. A hook echoing the ADR's quoted string
    verbatim produces `remote: remote: …`, and an exact-line assertion in WP-12
    then reds for a reason that has nothing to do with the classifier.
    """
    line = "GL-HOOK-ERR: You are not allowed to push code to this project."
    fake_forge.git_http_pre_receive_refusal = line
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT)

    result = _push(
        fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}", check=False
    )

    assert result.returncode != 0
    assert f"remote: {line}" in result.stderr
    assert f"remote: remote: {line}" not in result.stderr
    assert fake_forge.git_head(PROJECT, CLAIM_BRANCH) is None, (
        "the refusal must actually decline the update, not merely print a line"
    )


def test_server_side_rejection_texts_are_recorded(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The PRODUCER for WP-12's phrase table: every refusal shape, verbatim.

    `OBSERVED_REFUSAL_STDERR` ships as six empty strings. This test drives all
    six shapes against the local `git`, asserts every published value is
    non-empty, and asserts each value appears verbatim in the stderr that shape
    actually produced. An unfilled row therefore reds rather than passing
    vacuously, and a phrase git changes in a later release reds too — which is
    the drift the table exists to catch.

    Containment, not byte equality of the whole stream: git's stderr embeds the
    fixture's ephemeral port and object counts, so a whole-stream compare could
    never be deterministic. The published value is the phrase; the stream is
    where it must appear.

    `published in text` is ONE-directional, and two pairs of shapes overlap:
    `FORBIDDEN_RECEIVE_PACK`'s stderr contains `FORBIDDEN_INFO_REFS`'s published
    phrase verbatim, and `NOT_ALLOWED_TO_PUSH`'s contains
    `PRE_RECEIVE_DECLINED`'s. So a table that published one phrase under BOTH
    keys of either pair would pass this loop — which is precisely the state that
    destroys WP-12's ability to tell six shapes apart. Distinctness and the two
    reachable asymmetric guards are what close that.
    """
    observed = {shape: _refusal_stderr(fake_forge, tmp_path, shape) for shape in RefusalShape}

    assert set(observed) == set(RefusalShape), "a refusal shape went undriven"
    assert set(OBSERVED_REFUSAL_STDERR) == set(RefusalShape), (
        "the published table lost a row; a table keyed by an open-ended string "
        "could never notice a missing key"
    )
    assert len(set(OBSERVED_REFUSAL_STDERR.values())) == len(RefusalShape), (
        "two refusal shapes publish the same phrase; WP-12's classifier cannot "
        f"tell them apart: {OBSERVED_REFUSAL_STDERR!r}"
    )
    for shape, text in observed.items():
        published = OBSERVED_REFUSAL_STDERR[shape]
        assert published, (
            f"OBSERVED_REFUSAL_STDERR[{shape.value}] is unfilled — WP-12's phrase "
            f"table would be written against nothing. Observed stderr was:\n{text}"
        )
        assert published in text, (
            f"{shape.value}: the published phrase is not in the observed stderr.\n"
            f"published: {published!r}\nobserved:\n{text}"
        )

    # The two asymmetries. Each pair overlaps in ONE direction only, so only
    # these two of the four possible guards are reachable — the mirrored pair
    # (`FORBIDDEN_INFO_REFS`'s phrase absent from the receive-pack stream, and
    # `PRE_RECEIVE_DECLINED`'s absent from the not-allowed one) is FALSE against
    # real git 2.54 output and would red on the fixture being correct.
    assert (
        OBSERVED_REFUSAL_STDERR[RefusalShape.FORBIDDEN_RECEIVE_PACK]
        not in observed[RefusalShape.FORBIDDEN_INFO_REFS]
    ), (
        "the info/refs 403 stderr also carries the receive-pack phrase: the two "
        "403 shapes are indistinguishable and one table row covers for the other"
    )
    assert (
        OBSERVED_REFUSAL_STDERR[RefusalShape.NOT_ALLOWED_TO_PUSH]
        not in observed[RefusalShape.PRE_RECEIVE_DECLINED]
    ), (
        "the generic pre-receive refusal also carries GitLab's not-allowed line: "
        "the two hook shapes are then one shape published twice"
    )


def test_credential_helper_records_an_invocation(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The recording `credential.helper` really runs and really records.

    The positive control C-034's central assertion rests on. That assertion is
    `home.credential_calls() == []` — "the credential-injecting run reset the
    helper and invoked none" — and an empty list is ALSO what a helper git could
    not execute produces, silently: replaced by `exit 7` it records nothing and
    git says nothing at all, and left at mode `0644` git prints `Permission
    denied` on stderr and carries on. A `noexec` `TMPDIR`, or a lost `chmod` in
    `build_fixture_home`, puts the whole fixture in that state and every
    downstream "no helper was invoked" then passes against a broken helper.

    `git credential fill` is the smallest thing that invokes one. It exits
    non-zero here — nothing answers, and `GIT_TERMINAL_PROMPT=0` stops the
    fallback prompt — which is irrelevant: git runs the helper before it
    prompts, so the record is written either way.

    **`exit_status` is not asserted**, though it is captured: git always passes
    an operation, so the helper's `0 if action else 2` is `0` in every
    invocation git can produce, and the status is written before the exit, so a
    half-run helper leaves no record at all rather than one carrying a non-zero.
    An assertion on it could not go red in either direction. `len(calls) == 1`
    below is the real check and is what the C-034 negatives lean on.

    The `path=` row is asserted because it is not free: git strips the path from
    a credential description unless `credential.useHttpPath=true`, which
    `build_fixture_home` sets. Without it a recorded call names the host only,
    and a later test proving WHICH project's credential was requested has
    nothing to read.
    """
    _seeded(fake_forge, PROJECT)
    home = _home(tmp_path, credential_helper=True)
    assert home.helper is not None
    assert home.credential_log is not None
    assert home.credential_calls() == [], "the log is not empty before anything ran"

    host, port = _host_port(fake_forge)
    description = f"protocol=http\nhost={host}:{port}\npath={PROJECT}.git\n\n"
    subprocess.run(
        ["git", "credential", "fill"],
        input=description,
        env=git_environment(home.path),
        capture_output=True,
        text=True,
        timeout=HTTP_TIMEOUT_SECONDS,
        check=False,
    )

    calls = home.credential_calls()
    assert len(calls) == 1, (
        f"the recording helper was not invoked exactly once: {calls!r}. An empty "
        "list here means the helper is broken, not that nothing asked for a "
        "credential — and every C-034 assertion reads that same empty list"
    )
    assert calls[0].action == "get"
    assert f"host={host}:{port}" in calls[0].stdin
    assert f"path={PROJECT}.git" in calls[0].stdin, (
        "the description carries no path: `credential.useHttpPath` is not set, so "
        "a credential request cannot be attributed to a project.\n"
        f"stdin was:\n{calls[0].stdin}"
    )


# ── chunked request bodies (C-074) ───────────────────────────────────────────


def test_chunked_receive_pack_body_decoded(fake_forge: FakeForge, tmp_path: Path) -> None:
    """A chunked receive-pack body is decoded and handed to the backend intact.

    Framing is forced from the `HOME` side: `build_fixture_home(post_buffer=65536)`
    writes `http.postBuffer=65536`, and git streams chunked once a request exceeds
    `postBuffer - LARGE_PACKET_MAX`. The sibling route — `run_git(-c
    http.postBuffer=1)` — is exercised by
    `::test_chunked_framing_was_actually_observed`, so both declared knobs are
    covered and each test states which it used.

    **65536 is the floor for a `HOME`-side value, not a taste.** `LARGE_PACKET_MAX`
    is 65520, and any `HOME`-side `postBuffer` at or below it aborts every
    protocol-v2 fetch with `BUG: remote-curl.c:1533: The entire rpc->buf should be
    larger than LARGE_PACKET_MAX` — so the clone dies before this test can push at
    all. The per-invocation route is unaffected because a push speaks v0.

    A chunked push is always **two** receive-pack POSTs: git sends a `probe_rpc`
    first — a 4-byte `0000` `Content-Length` request — because a chunked stream
    cannot be replayed after a 401, so the auth handshake has to finish before the
    stream starts. The chunked request is therefore selected by IDENTITY, never by
    index; `posts[0]` is the probe.

    The assertion is on the recorded FRAMING and on the ref advancing, never on
    the push's success alone: a `Content-Length` push succeeds just as well and
    would pass a test named for the decoder without the decoder ever running.
    """
    _seeded(fake_forge, PROJECT)
    home = _home(tmp_path, post_buffer=65536)
    work = tmp_path / "work"
    _clone(fake_forge, PROJECT, home, work)
    _commit(work, home, {ROOT_PATH: ROOT_BYTES + b'{"claimed":true}\n'}, "claim")

    before = len(fake_forge.git_http_requests)
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}")

    posts = [
        request
        for request in fake_forge.git_http_requests[before:]
        if request.method == "POST" and request.service == "git-receive-pack"
    ]
    # The probe's shape is a premise elsewhere: `git_http_forbid_receive_pack`
    # steps over a body equal to `_RECEIVE_PACK_PROBE_BODY` so that it refuses
    # the pack rather than the handshake. Pinned here, on the one test that
    # already observes both POSTs, so a git that stopped sending a 4-byte
    # `Content-Length` probe reds rather than silently moving what that knob
    # refuses.
    assert len(posts) == 2, f"expected a probe and a pack POST, got {posts!r}"
    assert posts[0].framing is BodyFraming.CONTENT_LENGTH
    assert posts[0].body_bytes == 4, (
        f"git's receive-pack probe is no longer a 4-byte flush packet: {posts[0]!r}"
    )

    chunked = [request for request in posts if request.framing is BodyFraming.CHUNKED]
    assert len(chunked) == 1, (
        f"expected exactly one CHUNKED receive-pack POST among {posts!r} — "
        "zero means the decoder never ran and this test proves nothing"
    )
    assert chunked[0].body_bytes > 0
    assert chunked[0].rejection is None
    assert chunked[0].status == 200
    assert fake_forge.git_head(PROJECT, CLAIM_BRANCH) is not None, (
        "the decoded body must reach the backend intact: a truncated decode would "
        "leave the ref unset while the framing still read chunked"
    )


def test_chunked_framing_was_actually_observed(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The negative control for the whole chunked group.

    A claim root's pack is far below `http.postBuffer`'s 1 MiB default, so an
    ordinary push is `Content-Length`-framed. Two pushes differ in exactly one
    thing — `run_git(-c http.postBuffer=1)` on the second — and the recorded
    framing field must differ accordingly. Remove the override and both reads say
    `content-length` while both pushes still succeed.

    The forced arm records **two** POSTs, not one: git sends a `probe_rpc` — a
    4-byte `0000` `Content-Length` request to the same URL — before any chunked
    stream, because a chunked body cannot be replayed after a 401. Asserting a
    count of one there would red on git's own protocol rather than on framing, so
    the chunked request is taken by identity. The count that IS asserted is the
    default arm's, where a probe never happens.
    """
    _seeded(fake_forge, PROJECT)
    home = _home(tmp_path)
    work = tmp_path / "work"
    _clone(fake_forge, PROJECT, home, work)

    _commit(work, home, {ROOT_PATH: ROOT_BYTES + b'{"first":true}\n'}, "default framing")
    before = len(fake_forge.git_http_requests)
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}")
    default_posts = [
        request
        for request in fake_forge.git_http_requests[before:]
        if request.method == "POST" and request.service == "git-receive-pack"
    ]

    _commit(work, home, {ROOT_PATH: ROOT_BYTES + b'{"second":true}\n'}, "forced chunked")
    before = len(fake_forge.git_http_requests)
    run_git(
        "-C",
        str(work),
        *_push_credential(fake_forge, PROJECT),
        "-c",
        "http.postBuffer=1",
        "push",
        fake_forge.git_url(PROJECT),
        f"HEAD:refs/heads/{CLAIM_BRANCH}",
        home=home.path,
    )
    forced_posts = [
        request
        for request in fake_forge.git_http_requests[before:]
        if request.method == "POST" and request.service == "git-receive-pack"
    ]

    assert len(default_posts) == 1, f"expected one default-framed POST, got {default_posts!r}"
    assert default_posts[0].framing is BodyFraming.CONTENT_LENGTH

    forced_chunked = [
        request for request in forced_posts if request.framing is BodyFraming.CHUNKED
    ]
    assert len(forced_chunked) == 1, (
        f"expected exactly one CHUNKED POST under the override, got {forced_posts!r}"
    )
    assert not [
        request for request in default_posts if request.framing is BodyFraming.CHUNKED
    ], "the default arm framed chunked: the override is not what made the difference"


def test_forbidden_receive_pack_refuses_the_pack_not_the_probe(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """`git_http_forbid_receive_pack` steps over the handshake probe and answers
    403 to the POST that carries the pack.

    The step-over in `git_http_post` (`body not in (b"", _RECEIVE_PACK_PROBE_BODY)`)
    is the whole knob under chunked framing, and this is the only thing that
    defends it. A chunked push sends TWO receive-pack POSTs: git emits a 4-byte
    `0000` `Content-Length` probe first, because a chunked stream cannot be
    replayed after a 401, so the auth handshake has to finish before the stream
    starts. Delete the step-over and the knob fires on the probe, the pack POST
    never happens, and the knob silently changes meaning with the caller's
    framing — "the handshake was refused" under chunked, "the pack was refused"
    under `Content-Length`. A WP-17 assertion would believe it observed the
    second and have measured the first.

    `RPC failed; HTTP 403` reaches the client either way (measured against git
    2.54.0), so the client's stderr cannot tell them apart. The recorded framing
    and body length can, which is why this asserts on the log and not on the
    push's failure.

    `::test_chunked_receive_pack_body_decoded` pins the probe at four bytes —
    that is this step-over's PREMISE, not the step-over itself, and it passes
    unchanged with the step-over deleted.
    """
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT, name="forbid-chunked")
    fake_forge.git_http_forbid_receive_pack = True

    before = len(fake_forge.git_http_requests)
    # `-c` before the subcommand, so `run_git` directly rather than `_push`.
    # `postBuffer=1` is the same forced-chunked route
    # `::test_chunked_framing_was_actually_observed` uses.
    run_git(
        "-C",
        str(work),
        *_push_credential(fake_forge, PROJECT),
        "-c",
        "http.postBuffer=1",
        "push",
        fake_forge.git_url(PROJECT),
        f"HEAD:refs/heads/{CLAIM_BRANCH}",
        home=home.path,
        check=False,
    )
    posts = [
        request
        for request in fake_forge.git_http_requests[before:]
        if request.method == "POST" and request.service == "git-receive-pack"
    ]

    assert len(posts) == 2, (
        f"expected a probe POST and a pack POST, got {posts!r}. One POST means the "
        "403 landed on the handshake probe and git never sent the pack at all"
    )
    probe, pack = posts
    assert probe.framing is BodyFraming.CONTENT_LENGTH
    assert probe.body_bytes == 4
    assert probe.status == 200, (
        f"the probe was answered {probe.status}: the knob fired on the handshake "
        "rather than on the pack, and every assertion reading this knob under "
        "chunked framing measures the wrong refusal"
    )
    assert pack.framing is BodyFraming.CHUNKED
    assert pack.body_bytes > 4, (
        f"the refused POST carried {pack.body_bytes} bytes; that is the probe, "
        "not a pack"
    )
    assert pack.status == 403
    assert fake_forge.git_http_forbid_receive_pack is False, (
        "the knob stayed armed; it is one-shot like every other scripted failure "
        "in this fixture family"
    )
    assert fake_forge.git_head(PROJECT, CLAIM_BRANCH) is None, (
        "the refused push landed anyway: the 403 was written after the backend ran"
    )


def test_truncated_chunked_body_is_an_error(fake_forge: FakeForge, tmp_path: Path) -> None:
    """A stream that ends mid-body raises; it never returns the bytes it did get.

    Truncating instead of raising is the failure this strictness exists to
    prevent: a decoder returning the first chunk and dropping the rest makes the
    four-option assertion pass or fail for reasons unrelated to ocx.

    Driven twice — once through the pure decoder, once over a real half-closed
    socket — because the decoder's contract and the bridge's refusal record are
    two separate promises.
    """
    _seeded(fake_forge, PROJECT)
    truncated = b"20\r\n" + b"a" * 16

    with pytest.raises(ChunkedBodyError) as excinfo:
        read_chunked_body(io.BytesIO(truncated))
    assert excinfo.value.reason is ChunkedBodyRejection.TRUNCATED

    before = len(fake_forge.git_http_requests)
    reply = _chunked_post(
        fake_forge, _receive_pack_path(fake_forge, PROJECT), truncated, half_close=True
    )
    try:
        assert reply.status == 400
    finally:
        reply.probe.close()

    refused = fake_forge.git_http_requests[before:]
    assert len(refused) == 1, f"the refused request was not recorded: {refused!r}"
    assert refused[0].rejection is ChunkedBodyRejection.TRUNCATED
    assert refused[0].framing is BodyFraming.CHUNKED
    assert refused[0].status == 400
    assert dict(refused[0].cgi_env) == {}, "the CGI child must never have run"


def test_chunk_extension_is_refused(fake_forge: FakeForge, tmp_path: Path) -> None:
    """A chunk extension (`<size>;<ext>`) is refused, distinctly from its siblings.

    Real `git` never emits one, so this is reachable only from a hand-built
    stream. The rejection reason is asserted by name rather than "some 400",
    which could not tell a refused extension from a request the bridge rejected
    for an unrelated reason.
    """
    _seeded(fake_forge, PROJECT)
    body = b"5;name=value\r\nhello\r\n0\r\n\r\n"

    with pytest.raises(ChunkedBodyError) as excinfo:
        read_chunked_body(io.BytesIO(body))
    assert excinfo.value.reason is ChunkedBodyRejection.CHUNK_EXTENSION

    before = len(fake_forge.git_http_requests)
    reply = _chunked_post(fake_forge, _receive_pack_path(fake_forge, PROJECT), body)
    try:
        assert reply.status == 400
    finally:
        reply.probe.close()

    refused = fake_forge.git_http_requests[before:]
    assert len(refused) == 1
    assert refused[0].rejection is ChunkedBodyRejection.CHUNK_EXTENSION
    assert refused[0].framing is BodyFraming.CHUNKED
    assert dict(refused[0].cgi_env) == {}


def test_trailer_section_is_refused(fake_forge: FakeForge, tmp_path: Path) -> None:
    """A non-empty trailer section after the last chunk is refused.

    The companion to the extension refusal: both are legal RFC 9112 grammar that
    C-074 deliberately narrows, and each must be deletable one at a time to prove
    the other is not covering for it.
    """
    _seeded(fake_forge, PROJECT)
    body = b"5\r\nhello\r\n0\r\nX-Injected: value\r\n\r\n"

    with pytest.raises(ChunkedBodyError) as excinfo:
        read_chunked_body(io.BytesIO(body))
    assert excinfo.value.reason is ChunkedBodyRejection.TRAILER_SECTION

    before = len(fake_forge.git_http_requests)
    reply = _chunked_post(fake_forge, _receive_pack_path(fake_forge, PROJECT), body)
    try:
        assert reply.status == 400
    finally:
        reply.probe.close()

    refused = fake_forge.git_http_requests[before:]
    assert len(refused) == 1
    assert refused[0].rejection is ChunkedBodyRejection.TRAILER_SECTION
    assert refused[0].framing is BodyFraming.CHUNKED
    assert dict(refused[0].cgi_env) == {}


def test_oversized_chunked_body_is_capped(fake_forge: FakeForge, tmp_path: Path) -> None:
    """A body past the ceiling is refused under BOTH framings, never truncated.

    `git_http_max_body_bytes` is lowered per server so the cap is reachable
    without sending megabytes; the pure decoder is driven at the same ceiling
    through its `max_bytes` parameter.

    The `Content-Length` arm is here rather than in a test of its own because it
    is the same ceiling and the same refusal, and because its failure mode is
    invisible on its own: an earlier version clamped the declared length into
    `[0, git_http_max_body_bytes]` and read a prefix, answering a truncated body
    as if it were whole and leaving the remainder to frame the next request on a
    keep-alive socket. An unparsable length was coerced to `0` the same way.
    Three declarations exercise the three ways the arm can be lied to; a clamp
    restored on any of them answers 200 here.
    """
    _seeded(fake_forge, PROJECT)
    fake_forge.git_http_max_body_bytes = SMALL_BODY_CEILING_BYTES
    body = b"40\r\n" + b"a" * 64 + b"\r\n40\r\n" + b"a" * 64 + b"\r\n0\r\n\r\n"

    with pytest.raises(ChunkedBodyError) as excinfo:
        read_chunked_body(io.BytesIO(body), max_bytes=SMALL_BODY_CEILING_BYTES)
    assert excinfo.value.reason is ChunkedBodyRejection.OVERSIZED

    before = len(fake_forge.git_http_requests)
    reply = _chunked_post(fake_forge, _receive_pack_path(fake_forge, PROJECT), body)
    try:
        assert reply.status == 400
    finally:
        reply.probe.close()

    refused = fake_forge.git_http_requests[before:]
    assert len(refused) == 1
    assert refused[0].rejection is ChunkedBodyRejection.OVERSIZED
    assert dict(refused[0].cgi_env) == {}
    assert fake_forge.git_head(PROJECT, CLAIM_BRANCH) is None

    path = _receive_pack_path(fake_forge, PROJECT)
    oversized = b"a" * (SMALL_BODY_CEILING_BYTES * 2)
    declarations = (
        # Above the ceiling, refused on the DECLARED length before a byte of it
        # is read — the same rule the chunked arm applies to a chunk size. The
        # body is honestly sized so a restored clamp hands the backend a
        # truncated body and answers, rather than blocking on bytes the client
        # never promised; the assertion below is then about the refusal, not
        # about a socket timeout.
        (oversized, str(len(oversized)), ChunkedBodyRejection.OVERSIZED),
        # `int()` parses it, and `rfile.read(-1)` would then read to EOF —
        # unbounded in both time and memory.
        (b"0000", "-1", ChunkedBodyRejection.MALFORMED_SIZE),
        (b"0000", "not-a-number", ChunkedBodyRejection.MALFORMED_SIZE),
    )
    for body, declared, reason in declarations:
        before = len(fake_forge.git_http_requests)
        status = _content_length_post(fake_forge, path, body, declared=declared)
        assert status == 400, (
            f"`Content-Length: {declared}` was answered {status}; a clamped read "
            "returns a short body as if it were complete and leaves the rest to "
            "frame the next request on this socket"
        )
        recorded = fake_forge.git_http_requests[before:]
        assert len(recorded) == 1, f"the refusal was not recorded: {recorded!r}"
        assert recorded[0].framing is BodyFraming.CONTENT_LENGTH, (
            "the record names the chunked framing for a Content-Length refusal: "
            f"{recorded[0]!r}"
        )
        assert recorded[0].rejection is reason
        assert dict(recorded[0].cgi_env) == {}, "the CGI child must never have run"


def test_rest_seeding_is_refused_on_a_git_project(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The three REST seeders refuse a git-transport project; the route handlers
    do not.

    The shas the REST seeders mint are synthetic and a git project's are real,
    so mixing the two writers makes `branch_head` and `--force-with-lease`
    disagree silently — the guard exists to fail at that seam instead. It sits
    on the public seeders for a reason this test is the other half of:
    `handle_patch_ref`'s `concurrent_ref_advance` branch and `fake_gitlab`'s
    commit route both inject a racing writer through `_seed_files_locked`, and a
    guard below them would raise `GitFixtureError` inside a handler thread —
    a traceback on stderr and a dropped connection, which no client can read as
    a status.
    """
    _seeded(fake_forge, PROJECT)
    owner, _, repo = PROJECT.partition("/")

    for label, seed in (
        ("seed_files", lambda: fake_forge.seed_files(owner, repo, {"a.txt": b"x"})),
        ("seed_root", lambda: fake_forge.seed_root(owner, repo, "r.json", {"a": 1})),
        (
            "seed_branch_at",
            lambda: fake_forge.seed_branch_at(
                owner, repo, "copy", source_owner=owner, source_repo=repo
            ),
        ),
    ):
        with pytest.raises(GitFixtureError) as excinfo:
            seed()
        assert "is a git-transport project" in str(excinfo.value), (
            f"{label} raised something else: {excinfo.value!r}"
        )

    # The handler path: same project, same helper underneath, answered as a
    # status. A guard on `_seed_files_locked` drops this connection instead.
    fake_forge.concurrent_ref_advance[f"{PROJECT}/{INITIAL_BRANCH}"] = {
        "race.txt": b"a second writer landed here\n"
    }
    status, body = _http_patch(
        fake_forge,
        f"/repos/{PROJECT}/git/refs/heads/{INITIAL_BRANCH}",
        {"sha": "0" * 40, "force": False},
    )
    assert status == 422, f"the racing-advance branch answered {status}: {body!r}"
    assert "not a fast forward" in json.loads(body)["message"]


def test_refused_body_closes_the_connection(fake_forge: FakeForge, tmp_path: Path) -> None:
    """A refused chunked body ends the connection instead of leaving it desynced.

    The fixture speaks HTTP/1.1 with keep-alive, so a half-read body would leave
    unread bytes framed as the NEXT request on that socket — and the resulting
    failure surfaces inside an unrelated test. Both halves are asserted: the
    parsed `Connection: close` intent, and the socket actually reaching EOF.
    """
    _seeded(fake_forge, PROJECT)
    reply = _chunked_post(
        fake_forge,
        _receive_pack_path(fake_forge, PROJECT),
        b"5;name=value\r\nhello\r\n0\r\n\r\n",
    )
    try:
        assert reply.status == 400
        assert reply.will_close is True, (
            "the 400 did not announce `Connection: close`; the next request on this "
            "socket would be framed by the unread body"
        )
        assert reply.probe.recv(1) == b"", "the server kept the refused connection open"
    finally:
        reply.probe.close()


# ── seeding, filtering, logs ─────────────────────────────────────────────────


def test_seeded_branch_fixture_reaches_ahead_and_diverged(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """All six of C-051's branch states are reachable, and both stores agree.

    Each state gets its own project: `BEHIND` and `DIVERGED` advance the base, so
    six states on one project would make the loop order a hidden input.

    `AHEAD_IDENTICAL` is the state the whole enum exists for — a distinct commit
    carrying the base's tree byte for byte, which is what separates "nothing to
    do" from "nothing to commit but a request to ensure". Collapsing it into
    `IDENTICAL` erases exactly that case.

    The `branch_head` / `git_head` comparison is DX-5's claim: a REST read and a
    `--force-with-lease` must answer the same sha, and a single accessor could
    not express the question.
    """
    home = _home(tmp_path)
    branch = "ocx-announce"

    for state in BranchSeedState:
        project = f"ocx-sh/state-{state.value}"
        _seeded(fake_forge, project)
        work = tmp_path / f"work-{state.value}"
        _clone(fake_forge, project, home, work)

        seeded = fake_forge.git_seed_branch(
            project, branch, state, files={ROOT_PATH: ROOT_BYTES + b'{"differs":true}\n'}
        )
        run_git("-C", str(work), "fetch", "--all", home=home.path)

        head = fake_forge.git_head(project, branch)
        base = fake_forge.git_head(project, INITIAL_BRANCH)
        assert base is not None, f"{state.value}: the base branch vanished"
        assert seeded == head, f"{state.value}: the seeder returned a sha the repo does not hold"

        if state is BranchSeedState.ABSENT:
            assert head is None
            assert fake_forge.branch_head("ocx-sh", f"state-{state.value}", branch) is None
            continue

        assert head is not None
        owner, repo = project.split("/", 1)
        assert fake_forge.branch_head(owner, repo, branch) == head, (
            f"{state.value}: the in-memory graph and the bare repository disagree; "
            "every --force-with-lease assertion would measure the fixture"
        )

        def ancestor(older: str, newer: str, work: Path = work, home: FixtureHome = home) -> bool:
            return (
                run_git(
                    "-C",
                    str(work),
                    "merge-base",
                    "--is-ancestor",
                    older,
                    newer,
                    home=home.path,
                    check=False,
                ).returncode
                == 0
            )

        def same_tree(left: str, right: str, work: Path = work, home: FixtureHome = home) -> bool:
            return (
                run_git(
                    "-C", str(work), "diff", "--quiet", left, right, home=home.path, check=False
                ).returncode
                == 0
            )

        match state:
            case BranchSeedState.IDENTICAL:
                assert head == base
            case BranchSeedState.AHEAD_IDENTICAL:
                assert head != base
                assert ancestor(base, head)
                assert same_tree(base, head), (
                    "AHEAD_IDENTICAL must carry the base's tree UNCHANGED; a differing "
                    "tree makes it a second AHEAD_DIFFERING"
                )
            case BranchSeedState.AHEAD_DIFFERING:
                assert ancestor(base, head)
                assert not same_tree(base, head)
            case BranchSeedState.BEHIND:
                assert head != base
                assert ancestor(head, base)
            case BranchSeedState.DIVERGED:
                assert not ancestor(head, base)
                assert not ancestor(base, head)
            case _:
                raise AssertionError(f"unhandled branch state {state!r}")


def test_partial_clone_filter_is_honoured(fake_forge: FakeForge, tmp_path: Path) -> None:
    """`--filter=blob:none` really filters, proved against two controls.

    Without `uploadpack.allowFilter` the filter is SILENTLY ignored: the fetch
    still succeeds and only the missing-object count changes, so "the clone
    worked" is no evidence at all. Three arms — a full clone, a filtered clone,
    and a filtered clone against a project created without the config row — pin
    the count at zero, non-zero, and zero again.
    """
    _seeded(fake_forge, PROJECT)
    home = _home(tmp_path)

    def missing_objects(repo: Path) -> int:
        listing = run_git(
            "-C",
            str(repo),
            "rev-list",
            "--objects",
            "--all",
            "--missing=print",
            home=home.path,
            extra_env={"GIT_NO_LAZY_FETCH": "1"},
        ).stdout
        return sum(1 for line in listing.splitlines() if line.startswith("?"))

    full = _clone(fake_forge, PROJECT, home, tmp_path / "full")
    assert (full / ROOT_PATH).read_bytes() == ROOT_BYTES
    assert missing_objects(full) == 0

    filtered = _clone(
        fake_forge, PROJECT, home, tmp_path / "filtered", "--filter=blob:none", "--no-checkout"
    )
    assert missing_objects(filtered) > 0, (
        "the blobless clone holds every object: the filter was negotiated away or "
        "silently ignored, and C-036's cost rationale is untested"
    )

    unfiltered_project = "ocx-sh/no-filter"
    config = {key: value for key, value in BARE_REPO_CONFIG.items() if key != "uploadpack.allowFilter"}
    _seeded(fake_forge, unfiltered_project, config=config)
    control = _clone(
        fake_forge,
        unfiltered_project,
        home,
        tmp_path / "control",
        "--filter=blob:none",
        "--no-checkout",
    )
    assert missing_objects(control) == 0, (
        "dropping uploadpack.allowFilter must make the filter a no-op; if this "
        "still reports missing objects the count above is not measuring the filter"
    )


def test_git_requests_are_not_recorded_in_the_rest_log(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """Git traffic goes in its own log; `requests` is untouched.

    `request_count` is exact-matched at 28 call sites in `test_announce.py`, and
    S-026's "zero REST writes on every git failure path" is asserted against it.
    Folding clone/fetch/push requests in would turn that into an unfalsifiable
    assertion — silently, because the announce suite would still pass.
    """
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT)
    rest_before = list(fake_forge.requests)
    auth_before = len(fake_forge.auth_headers)
    git_before = len(fake_forge.git_http_requests)

    _clone(fake_forge, PROJECT, home, tmp_path / "second-clone")
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}")

    assert len(fake_forge.git_http_requests) > git_before, (
        "no git request was recorded at all; the negative below would be vacuous"
    )
    assert list(fake_forge.requests) == rest_before, (
        "a git request reached the REST log"
    )
    assert len(fake_forge.auth_headers) == auth_before
    assert len(fake_forge.auth_headers) == len(fake_forge.requests), (
        "auth_headers must stay aligned with requests; a git request appending to "
        "one and not the other silently shifts every later index"
    )


# ── the CGI bridge ───────────────────────────────────────────────────────────


def test_cgi_status_line_demotes_and_is_stripped() -> None:
    """`Status:` decides the response code and never becomes a response header.

    This is the ONE wire-format deviation `git_http_fixture.py` hand-owns — CGI's
    `Status:` demotion, RFC 3875 §6.3.3 — and `quality-core.md` § *Don't Own
    Non-Domain Code* puts a hand-written codec at Block tier. Everything else in
    `split_cgi_response` is delegated to `http.client.parse_headers`.

    It needs a test of its own because the demotion is **unreachable through the
    server**: `_git_http_route` answers None for an unregistered project, so the
    missing-repository shape that would make `http-backend` emit a `Status:` line
    never arrives, and no test drives a dumb `objects/**` path. Every 400 and 403
    this suite asserts is written by `_git_http_reply` directly, without ever
    passing through here. The function is exported and pure, so the check costs
    no server.

    Stripping is asserted, not just the code: `http-backend` exits 0 whatever it
    emitted, so a forwarded `Status:` header would ride out to the client as a
    second, contradictory statement of the result.
    """
    status, headers, body = split_cgi_response(
        b"Status: 404 Not Found\r\nContent-Type: text/plain\r\n\r\nno such repo\n"
    )
    assert status == 404
    assert "Status" not in headers, (
        "the Status line was forwarded as a response header; it is CGI's channel "
        "for the code, not a header the client should ever see"
    )
    assert headers.get("Content-Type") == "text/plain"
    assert body == b"no such repo\n"

    # The default arm, and the reason the first one is not vacuous: absent a
    # `Status:` line the answer is 200, so a parser that returned 404 for
    # everything would red here.
    ok_status, ok_headers, ok_body = split_cgi_response(
        b"Content-Type: application/x-git-upload-pack-advertisement\r\n\r\n0000"
    )
    assert ok_status == 200
    assert ok_headers.get("Content-Type") == "application/x-git-upload-pack-advertisement"
    assert ok_body == b"0000"


def test_git_protocol_header_reaches_the_backend(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """Every request header is forwarded as `HTTP_<NAME>`, `Git-Protocol` included.

    A bridge passing a hard-coded env set downgrades every request to protocol
    v0, and capability-dependent behaviour then differs from a real server
    invisibly. Both ends are asserted: `cgi_env` must carry
    `HTTP_GIT_PROTOCOL`, and the advertisement itself must be the v2 one — with
    the header-less request as the negative control, since a hard-coded bridge
    answers v0 to both.
    """
    _seeded(fake_forge, PROJECT)
    path = urllib.parse.urlsplit(fake_forge.git_url(PROJECT)).path + "/info/refs?service=git-upload-pack"

    before = len(fake_forge.git_http_requests)
    status, body = _http_get(fake_forge, path, {GIT_PROTOCOL_HEADER: "version=2"})
    negotiated = fake_forge.git_http_requests[before:]

    before = len(fake_forge.git_http_requests)
    plain_status, plain_body = _http_get(fake_forge, path)
    bare = fake_forge.git_http_requests[before:]

    assert status == 200
    assert plain_status == 200
    assert len(negotiated) == 1, f"expected one recorded info/refs, got {negotiated!r}"
    assert len(bare) == 1

    assert _header(negotiated[0], GIT_PROTOCOL_HEADER) == "version=2"
    assert negotiated[0].cgi_env.get("HTTP_GIT_PROTOCOL") == "version=2", (
        "the header arrived but was not forwarded: the bridge passes a curated "
        "env set, so every request runs at protocol v0"
    )
    assert _header(bare[0], GIT_PROTOCOL_HEADER) is None
    assert "HTTP_GIT_PROTOCOL" not in bare[0].cgi_env

    assert b"version 2" in body
    assert b"ls-refs" in body, "the v2 advertisement carries no ls-refs capability"
    assert b"version 2" not in plain_body, (
        "the header-less request also answered v2; the two requests are "
        "indistinguishable and the assertion above proves nothing"
    )


def test_bridge_stderr_is_surfaced(fake_forge: FakeForge, tmp_path: Path) -> None:
    """`http-backend`'s stderr is captured verbatim, never swallowed.

    The process exits 0 whatever `Status:` it emitted, so its exit code carries
    no signal at all and stderr is the only diagnosis a failing request has. A
    successful request is the control: if `backend_stderr` were non-empty in
    every state, "the failure was surfaced" would prove nothing.
    """
    _seeded(fake_forge, PROJECT)
    base = urllib.parse.urlsplit(fake_forge.git_url(PROJECT)).path

    before = len(fake_forge.git_http_requests)
    _http_get(fake_forge, f"{base}/info/refs?service=git-upload-pack")
    healthy = fake_forge.git_http_requests[before:]

    before = len(fake_forge.git_http_requests)
    _http_post(
        fake_forge,
        f"{base}/git-receive-pack",
        b"this is not a pkt-line stream",
        "application/x-git-receive-pack-request",
        authorized=True,
    )
    broken = fake_forge.git_http_requests[before:]

    assert len(healthy) == 1, f"expected one healthy request, got {healthy!r}"
    assert len(broken) == 1, f"expected one broken request, got {broken!r}"
    assert healthy[0].status == 200
    assert healthy[0].backend_stderr == "", (
        "a clean request produced backend stderr; the field is then non-empty in "
        "every state and cannot diagnose anything"
    )
    assert broken[0].backend_stderr != "", (
        "a malformed receive-pack body produced no captured stderr: the child's "
        "only diagnostic channel is being swallowed"
    )


def test_fixture_shuts_down_with_a_request_in_flight(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The fixture cannot wedge a run: a half-sent body is ABANDONED, not held.

    `git http-backend` streams with no `Content-Length`; under HTTP/1.1
    keep-alive an unframed response hangs the client, and there is no
    `pytest-timeout` in `test/pyproject.toml` to rescue the run. The property
    that makes teardown safe is that a handler blocked on an unfinished body
    gives up by itself.

    **`assert not stopper.is_alive()` used to stand here and could not go red.**
    `ThreadingHTTPServer.daemon_threads` is True, and `socketserver._Threads.append`
    returns early for daemon threads, so `server_close()`'s join never sees a
    blocked handler at all — the assertion passes under every implementation of
    `_git_http_read_body`, including one that blocks forever. What IS reachable
    is the client's own socket: with `git_http_body_read_timeout_seconds` lowered
    to something a test can outwait, the bridge must close the connection. Raise
    the knob back to its 30s default and this test hangs to its bound and fails.

    Both halves are observations. The `TimeoutError` says the handler was still
    reading when the client looked — the request really was in flight — and the
    EOF says it stopped.
    """
    fake_forge.git_http_body_read_timeout_seconds = SHORT_BODY_READ_TIMEOUT_SECONDS
    _seeded(fake_forge, PROJECT)
    host, port = _host_port(fake_forge)
    conn = http.client.HTTPConnection(host, port, timeout=HTTP_TIMEOUT_SECONDS)
    conn.connect()
    try:
        conn.putrequest("POST", _receive_pack_path(fake_forge, PROJECT))
        conn.putheader("Content-Type", "application/x-git-receive-pack-request")
        conn.putheader("Transfer-Encoding", "chunked")
        conn.endheaders()
        conn.send(b"20\r\n" + b"a" * 32 + b"\r\n")

        # Well inside the server's own window: nothing may have been answered
        # yet, which is what makes the request "in flight" rather than done.
        conn.sock.settimeout(SHORT_BODY_READ_TIMEOUT_SECONDS / 4)
        with pytest.raises(TimeoutError):
            conn.sock.recv(1)

        conn.sock.settimeout(HTTP_TIMEOUT_SECONDS)
        started = time.monotonic()
        assert conn.sock.recv(1) == b"", (
            "the server answered the half-sent body instead of abandoning it"
        )
        waited = time.monotonic() - started
        assert waited < ABANDON_BOUND_SECONDS, (
            f"the connection was abandoned only after {waited:.2f}s against a "
            f"{SHORT_BODY_READ_TIMEOUT_SECONDS}s window: the timeout is not the "
            "thing that ended it"
        )
    finally:
        conn.close()


# ── redirects and credentials ────────────────────────────────────────────────


def _arm_redirect_to_sibling(fake_forge: FakeForge, tmp_path: Path) -> FixtureHome:
    """Two projects on one host, with the index's next `info/refs` redirected to
    the sibling.

    The target is built from `git_url` so it is a REAL sibling project: a
    synthetic path could not match the credential's URL prefix under any
    implementation, which would make "no `Authorization` reached it" true by
    construction.
    """
    _seeded(fake_forge, PROJECT)
    _seeded(fake_forge, SIBLING)
    fake_forge.git_http_redirect_info_refs = (
        f"{fake_forge.git_url(SIBLING)}/info/refs?service=git-upload-pack"
    )
    return _home(tmp_path)


def test_redirect_target_is_reachable(fake_forge: FakeForge, tmp_path: Path) -> None:
    """The redirect target is a real, reachable project — not a synthetic path.

    An absence assertion over zero requests is vacuous in both directions, so the
    pair this test opens starts by proving the sibling is genuinely served: with
    `http.followRedirects=true` the replay must land there and succeed.
    """
    home = _arm_redirect_to_sibling(fake_forge, tmp_path)

    before = len(fake_forge.git_http_requests)
    result = run_git(
        "-c",
        "http.followRedirects=true",
        "ls-remote",
        fake_forge.git_url(PROJECT),
        home=home.path,
    )
    served = fake_forge.git_http_requests[before:]

    assert result.returncode == 0
    sibling_requests = [request for request in served if request.project == SIBLING]
    assert sibling_requests, (
        "the redirect target received no request at all; every later assertion "
        "about what did or did not reach the sibling would be vacuous"
    )
    assert any(request.status == 200 for request in sibling_requests), (
        "the sibling answered nothing successfully: an unreachable target proves "
        "nothing about credential scope"
    )


def test_post_redirect_target_is_reachable(fake_forge: FakeForge, tmp_path: Path) -> None:
    """The POST-side redirect knob really redirects, to a project that answers.

    `git_http_redirect_receive_pack` shipped with **zero** consumers while its
    `info/refs` twin has both halves of its reachability pair. WP-17 owns S-039
    but cannot add a fixture knob — its declared files are `test_transport_git.py`
    and `announce_helpers.py` — so the knob has to be proved here or not at all,
    and an absence assertion written against an unexercised knob is vacuous by
    construction.

    The knob exists separately from the `info/refs` one because a single "next
    git request" knob can never reach this shape: a push's FIRST request is
    `GET /info/refs?service=git-receive-pack`, and C-068's
    `followRedirects=false` aborts the push right there.

    What is asserted is reachability, and nothing about the push's outcome.
    Whether the redirected request keeps its method, whether the pack is
    replayed, and whether git then rewrites the remote base are all git's and
    libcurl's redirect semantics — observed to vary — and pinning them here
    would red on a curl upgrade rather than on the knob. What S-039 needs from
    this fixture is that the Location was honoured and the sibling's own
    receive-pack route saw the request.
    """
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT, name="post-redirect")
    _seeded(fake_forge, SIBLING)
    fake_forge.git_http_redirect_receive_pack = (
        f"{fake_forge.git_url(SIBLING)}/git-receive-pack"
    )

    before = len(fake_forge.git_http_requests)
    # `-c` before the subcommand, so `run_git` directly rather than `_push`,
    # whose extra arguments land after `push` where git rejects them.
    run_git(
        "-C",
        str(work),
        *_push_credential(fake_forge, PROJECT),
        "-c",
        "http.followRedirects=true",
        "push",
        fake_forge.git_url(PROJECT),
        f"HEAD:refs/heads/{CLAIM_BRANCH}",
        home=home.path,
        check=False,
    )
    served = fake_forge.git_http_requests[before:]

    redirected = [
        request
        for request in served
        if request.project == PROJECT and request.method == "POST" and request.status == 302
    ]
    assert len(redirected) == 1, (
        f"the receive-pack POST was not answered with a 302: {served!r}. The knob "
        "fired on nothing, so nothing was redirected"
    )
    assert fake_forge.git_http_redirect_receive_pack is None, (
        "the knob stayed armed; it is one-shot like every other scripted failure "
        "in this fixture family, and a test needing a persistent redirect re-arms"
    )

    sibling_requests = [request for request in served if request.project == SIBLING]
    assert sibling_requests, (
        "the redirect target received no request at all; every later assertion "
        "about what did or did not reach the sibling would be vacuous"
    )
    assert any(
        request.path.endswith("/git-receive-pack") for request in sibling_requests
    ), (
        "the sibling was reached, but not on the route the Location named: the "
        f"redirect went somewhere else. {[r.path for r in sibling_requests]!r}"
    )


def test_authorization_reaches_the_index_project(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The index project's own requests DO carry the injected header.

    The confirm half of the pair: `http.<prefix>.extraHeader` is only meaningful
    if it actually arrives, and `HTTP_AUTHORIZATION` is the sole server-side
    observation point for it. Without this, "the sibling saw no `Authorization`"
    (S-040, WP-17) is satisfied by a header that reached nobody.
    """
    home = _arm_redirect_to_sibling(fake_forge, tmp_path)
    index_url = fake_forge.git_url(PROJECT)

    before = len(fake_forge.git_http_requests)
    run_git(
        "-c",
        "http.followRedirects=true",
        "-c",
        f"http.{index_url}.extraHeader=Authorization: {FAKE_AUTHORIZATION}",
        "ls-remote",
        index_url,
        home=home.path,
    )
    served = fake_forge.git_http_requests[before:]

    index_requests = [request for request in served if request.project == PROJECT]
    assert index_requests, "no request reached the index project"
    assert all(
        _header(request, "Authorization") == FAKE_AUTHORIZATION for request in index_requests
    ), (
        "the injected header did not reach the project it was scoped to: "
        f"{[_header(request, 'Authorization') for request in index_requests]!r}"
    )


def _receive_pack_requests(fake_forge: FakeForge, since: int) -> list[GitRequest]:
    """Every receive-pack request — advertisement and RPC — recorded since `since`."""
    return [
        request
        for request in fake_forge.git_http_requests[since:]
        if request.service == "git-receive-pack"
    ]


def test_receive_pack_refuses_a_missing_and_a_wrong_credential(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The receive-pack path demands HTTP Basic, and the client surfaces the
    refusal.

    **This is the row that makes every credential assertion in this fixture
    family mean something.** Those rows read the pushing process's own
    environment for the credential it resolved; none of them can tell that the
    credential was *needed*. With the bridge serving `git-receive-pack` to
    anyone, a build that resolved a credential and then failed to put it on the
    wire pushed exactly as successfully as one that did — so "the push landed"
    was evidence of nothing, and the wire half of every ladder claim was
    unfalsifiable.

    Three arms, and the third is not decoration: two refusals with no accepted
    push beside them are equally true of a bridge that refuses every push for an
    unrelated reason, which is the always-red twin of the always-green defect
    this row exists to close.

    The two refusals differ in exactly one thing — whether the credential is
    absent or merely not the accepted one — and the second is only reachable
    because `git_http_credential` gives the bridge an opinion about which
    secret is right. Without that knob "a wrong credential" has no state.

    `GIT_TERMINAL_PROMPT=0` (`git_environment`) is what turns the 401 into a
    fatal error rather than a prompt: git consults a helper, the fixture `HOME`
    here has none, and the prompt it would fall back to is disabled.
    """
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT, name="credential")
    refspec = f"HEAD:refs/heads/{CLAIM_BRANCH}"

    before = len(fake_forge.git_http_requests)
    anonymous = run_git(
        "-C", str(work), "push", fake_forge.git_url(PROJECT), refspec,
        home=home.path, check=False,
    )
    unauthenticated = _receive_pack_requests(fake_forge, before)

    fake_forge.git_http_credential = ("ocx", "the-only-secret-this-server-accepts")
    before = len(fake_forge.git_http_requests)
    mismatched = _push(fake_forge, PROJECT, work, home, refspec, check=False)
    rejected = _receive_pack_requests(fake_forge, before)

    fake_forge.git_http_credential = None
    accepted = _push(fake_forge, PROJECT, work, home, refspec)

    for name, result, requests in (
        ("no credential", anonymous, unauthenticated),
        ("a credential the server does not accept", mismatched, rejected),
    ):
        assert result.returncode != 0, (
            f"a push with {name} was ACCEPTED: the receive-pack path serves "
            "anyone, and every credential assertion in this family is vacuous"
        )
        assert requests, f"the push with {name} reached the receive-pack route not at all"
        assert all(request.status == 401 for request in requests), (
            f"the push with {name} was refused, but not as an auth failure: "
            f"{[(request.method, request.status) for request in requests]}"
        )
        assert AUTH_FAILURE_NEEDLE in result.stderr, (
            f"the client did not surface the refusal as an auth failure. "
            f"stderr was:\n{result.stderr}"
        )

    assert accepted.returncode == 0, (
        f"the same push WITH an accepted credential must land, or the two "
        f"refusals above are about something else entirely: {accepted.stderr}"
    )
    assert fake_forge.git_head(PROJECT, CLAIM_BRANCH) is not None, (
        "the accepted push reported success but landed no ref"
    )


def _upload_pack_requests(fake_forge: FakeForge, since: int) -> list[GitRequest]:
    """Every upload-pack request recorded since `since`."""
    return [
        request
        for request in fake_forge.git_http_requests[since:]
        if request.service == "git-upload-pack"
    ]


def test_fetch_refuses_a_wrong_credential_with_401_and_a_forbidden_project_with_403(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The READ half refuses too, and refuses the two states with different
    statuses.

    The write half's gate is not enough on its own. `require_credential` refuses
    a credential-less ocx write at exit 80 *before* `git` starts, so no ocx run
    can ever be observed meeting a receive-pack 401 — the fetch at
    `GitWorkspace::open` is the first and only place a resolved-but-wrong secret
    reaches a server. A bridge that authenticated only the push would leave
    `git_stderr.rs`'s whole `CREDENTIAL_REJECTIONS` table unreachable from an
    end-to-end row, and exit 80 for a rejected credential provable only in a unit
    test of the classifier's own literals.

    The two statuses are not interchangeable and each has one needle: 401 is
    `could not read Username for` (git found nothing to answer the challenge
    with), 403 is libcurl's `The requested URL returned error: 403`. Collapsing
    them would leave whichever survived matching both, and a build that lost the
    403 row would still pass.

    The fourth arm is the negative control and is why the three refusals mean
    anything: the same clone, with the accepted credential and nothing armed,
    must succeed. Three refusals alone are equally true of a bridge that refuses
    every read.

    Mutations: return `True` unconditionally from `_git_http_authorize`'s
    non-writing arm (the first three arms red); answer 401 rather than 403 for
    `git_http_forbid_fetch` (the third reds on its status).
    """
    _seeded(fake_forge, PROJECT)
    home = _home(tmp_path, "home-fetch-auth")
    url = fake_forge.git_url(PROJECT)

    # A private project: the read half demands a credential too. Off by default,
    # because a public project's `upload-pack` is served to anyone.
    fake_forge.git_http_private = True
    fake_forge.git_http_credential = ("ocx", "fake-token")

    before = len(fake_forge.git_http_requests)
    anonymous = run_git("clone", url, str(tmp_path / "anonymous"), home=home.path, check=False)
    unauthenticated = _upload_pack_requests(fake_forge, before)

    fake_forge.git_http_credential = ("ocx", "a-secret-no-client-here-carries")
    before = len(fake_forge.git_http_requests)
    mismatched = run_git(
        *_push_credential(fake_forge, PROJECT),
        "clone", url, str(tmp_path / "mismatched"),
        home=home.path, check=False,
    )
    rejected = _upload_pack_requests(fake_forge, before)

    fake_forge.git_http_credential = ("ocx", "fake-token")
    fake_forge.git_http_forbid_fetch = True
    before = len(fake_forge.git_http_requests)
    forbidden = run_git(
        *_push_credential(fake_forge, PROJECT),
        "clone", url, str(tmp_path / "forbidden"),
        home=home.path, check=False,
    )
    refused = _upload_pack_requests(fake_forge, before)

    fake_forge.git_http_forbid_fetch = False
    accepted = run_git(
        *_push_credential(fake_forge, PROJECT),
        "clone", url, str(tmp_path / "accepted"),
        home=home.path, check=False,
    )

    for name, result, requests, status in (
        ("no credential", anonymous, unauthenticated, 401),
        ("a credential the server does not accept", mismatched, rejected, 401),
        ("a credential the project forbids", forbidden, refused, 403),
    ):
        assert result.returncode != 0, (
            f"a fetch with {name} SUCCEEDED: the read half serves anyone, and no "
            f"ocx row can observe a rejected credential at all"
        )
        assert requests, f"the fetch with {name} reached the upload-pack route not at all"
        assert all(request.status == status for request in requests), (
            f"the fetch with {name} was refused, but not with {status}: "
            f"{[(request.method, request.status) for request in requests]}"
        )
        assert FETCH_REFUSAL_NEEDLES[status] in result.stderr, (
            f"the client did not surface the {status} the way `git_stderr.rs` "
            f"recovers it. stderr was:\n{result.stderr}"
        )

    assert accepted.returncode == 0, (
        f"the same clone WITH the accepted credential and nothing armed must "
        f"succeed, or the three refusals above are about something else "
        f"entirely: {accepted.stderr}"
    )
    assert (tmp_path / "accepted" / ROOT_PATH).exists(), (
        "the accepted clone reported success but carried no content"
    )


def test_merge_request_poll_reads_more_than_once(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """A merge request that is not ready yet really makes the poll poll.

    A synchronous fixture means the poll never polls: the first read already
    answers, every "the request was confirmed" assertion passes, and the retry
    path the ADR worries about is never exercised. So the first read must be
    empty and a later one must answer.

    **Neither half is timed.** `ready_at` is stamped by the `post-receive` hook
    and evaluated at read time, so a small positive delay makes the first read a
    race between the wall clock and a push plus an HTTP round trip — green here,
    and red on a loaded runner for a reason that has nothing to do with polling.
    The first push therefore carries S-022's `None`, which is "never ready" by
    construction, and readiness is made to arrive by a SECOND push carrying
    `0.0`. Nothing a slow machine can do moves either outcome. Give the first
    push `0.0` instead and the first read answers it, which reds the assertion
    below.

    The read loop is bounded only so that a fixture which never promotes fails
    instead of hanging. The read COUNT is deliberately not asserted: this test
    performs the first read and at least one more itself, so `> 1` holds
    whenever the two assertions below hold and could never go red.
    """
    fake_forge.git_http_merge_request_delay = None
    home, work = _claim_push_ready(fake_forge, tmp_path, PROJECT)
    project_id = fake_forge.gl_project_id(PROJECT)

    options: list[str] = []
    for option in CLAIM_PUSH_OPTIONS:
        options += ["-o", option]
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}", *options)

    path = (
        f"/projects/{project_id}/merge_requests"
        f"?source_branch={CLAIM_BRANCH}&source_project_id={project_id}"
    )
    _, first = _http_get(fake_forge, path)
    assert json.loads(first) == [], (
        "the merge request was already visible on the first read: the fixture is "
        "synchronous and the poll under test never polls"
    )

    # What makes readiness arrive is a second push, not elapsed time: its own
    # `ready_at` is `now + 0.0`, so the very next read promotes it.
    fake_forge.git_http_merge_request_delay = 0.0
    _commit(work, home, {ROOT_PATH: ROOT_BYTES + b'{"ready":true}\n'}, "ready now")
    _push(fake_forge, PROJECT, work, home, f"HEAD:refs/heads/{CLAIM_BRANCH}", *options)

    deadline = time.monotonic() + POLL_DEADLINE_SECONDS
    listed: list = []
    while True:
        _, payload = _http_get(fake_forge, path)
        listed = json.loads(payload)
        if listed or time.monotonic() >= deadline:
            break
        time.sleep(0.05)

    assert listed, f"no merge request became visible within {POLL_DEADLINE_SECONDS}s"
    assert listed[0]["source_branch"] == CLAIM_BRANCH


# ── the REST identity surface (DX-9) ─────────────────────────────────────────


def test_auth_headers_for_keeps_one_entry_per_request(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """A headerless read is an ENTRY, not a gap.

    C-026's three outcomes are told apart by which auth header a read carried,
    and its empty-credential arm asserts that a read carried NONE. That arm needs
    `{}` at the position of the read it is about — an accessor that skipped
    headerless requests would answer one element where two reads were made, and
    the assertion would then be reading the wrong request's headers while still
    passing. Two reads of one endpoint, one with a credential and one without,
    is the smallest shape that separates the two implementations.

    Values are lists because `record` keeps every value of a repeated name; a
    header sent twice is a regression worth seeing, not one worth keeping half of.
    """
    before = len(fake_forge.auth_headers_for("GET", "/user"))

    _http_get(fake_forge, "/user", {"PRIVATE-TOKEN": "fake-private-token"})
    _http_get(fake_forge, "/user")

    captured = fake_forge.auth_headers_for("GET", "/user")[before:]
    assert captured == [{"PRIVATE-TOKEN": ["fake-private-token"]}, {}], (
        "the headerless read is missing or misaligned; C-026's empty-credential "
        f"arm reads this position: {captured!r}"
    )
    assert fake_forge.auth_headers_for("GET", "/users/alice") == [], (
        "an endpoint nobody read answered a non-empty list; the filter is not "
        "filtering on the path"
    )


def test_a_deferred_stub_answers_501_with_its_message(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """A stub reached over HTTP is a 501 carrying its sentence, not a traceback.

    The subject is the MECHANISM, not any one knob: raising inside a handler
    thread is not how a fixture says "not implemented" — `socketserver` prints a
    traceback to the test's stderr and drops the connection, so the consumer
    that arms the knob sees a socket error and has to read the fixture to find
    out why. `_Handler._reply_stub` turns it into a 501 carrying the sentence.

    **The probe moved from `/user` to `/users/<login>` (DX-76).** WP-16 armed
    `handle_get_authenticated_user`'s three arms and `gl_get_users`' one, so
    `/user` now answers 403 for real and this assertion would red on a fixture
    that is working as intended. `handle_get_user_by_login`'s arm is the one
    that stays deferred — by contract, not by schedule: DX-72 established that
    `UsersApiUnavailable` is unreachable on GitHub, so no status this knob could
    answer there produces the error it is named for. Pointing the probe at it
    keeps this mechanism assertion alive without pinning it to a knob some later
    package will legitimately implement.
    """
    fake_forge.users_api_status = 403

    status, body = _http_get(fake_forge, "/users/alice")

    assert status == 501, (
        f"a deferred stub answered {status}; a consumer arming this knob cannot "
        "tell 'the fixture does not implement this yet' from a real answer"
    )
    assert "UsersApiUnavailable" in json.loads(body)["message"], (
        f"the stub's own sentence did not reach the caller: {body!r}"
    )
    assert fake_forge.request_count("GET", "/users/alice") == 1, (
        "the request was not recorded; a stub that drops the connection loses "
        "the read as well as the answer"
    )


def test_user_lookup_answers_the_canonical_spelling(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """`GET /users/AliCe` resolves and answers `alice` — C-048's confirm step.

    The transformation is the point, and it has two distinct red states. A
    case-SENSITIVE store 404s the caller's spelling, so the owner looks unknown
    and exit 79 fires against an account that exists. A store that echoed the
    caller's spelling back answers `AliCe`, which turns the confirm step into an
    echo — the run then "confirms" whatever it was given.

    An unseeded login must be a 404 rather than anything else: the client reads
    that as `Ok(None)` and `OwnerUnknown` is decided above it, so a fixture
    answering differently would move the decision into the fixture.
    """
    seeded = fake_forge.seed_user("alice", 7)
    assert seeded.login == "alice"
    assert seeded.bot is False

    status, body = _http_get(fake_forge, "/users/AliCe")
    assert status == 200, f"the case-folded lookup 404d: {body!r}"
    payload = json.loads(body)
    assert payload["login"] == "alice", (
        "the forge echoed the caller's spelling instead of its own; C-048's "
        f"confirm step then confirms nothing: {payload!r}"
    )
    assert payload["id"] == 7
    assert payload["type"] == "User"

    missing_status, _ = _http_get(fake_forge, "/users/nobody")
    assert missing_status == 404


def test_gitlab_user_lookup_answers_a_list(fake_forge: FakeForge, tmp_path: Path) -> None:
    """`GET /users?username=` answers a LIST, empty for an unknown login (C-028).

    Three things have to hold together and each has its own red state. The body
    is a list — a dict-shaped answer deserialises for a client that reads one
    field and diverges from the real API only once a second match exists. An
    unknown login is `200 []`, never a 404, because `Ok(None)` is what the
    client turns into `OwnerUnknown` and a 404 would move that decision here.
    And the lookup case-folds onto the same account store GitHub's route uses,
    so one seeded user confirms identically on both surfaces.
    """
    fake_forge.seed_user("alice", 7)

    status, body = _http_get(fake_forge, "/users?username=AliCe")
    assert status == 200
    listed = json.loads(body)
    assert isinstance(listed, list), f"the lookup answered a non-list body: {listed!r}"
    assert listed == [{"username": "alice", "id": 7, "bot": False}]

    empty_status, empty_body = _http_get(fake_forge, "/users?username=nobody")
    assert empty_status == 200, (
        "an unknown login answered a status instead of an empty collection; "
        "`Ok(None)` is what the client turns into OwnerUnknown, and deciding it "
        "here moves the decision into the fixture"
    )
    assert json.loads(empty_body) == []


# ── the recording git shim ───────────────────────────────────────────────────


def _shim_env(shim, home: FixtureHome, **extra: str) -> dict[str, str]:
    env = {
        "PATH": shim.child_path(),
        "HOME": str(home.path),
        "GIT_CONFIG_NOSYSTEM": "1",
        "LC_ALL": "C",
        "LANGUAGE": "",
    }
    env.update(extra)
    return env


def test_shim_records_argv_and_env_then_delegates(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The shim records the invocation, runs the real `git`, and lives only under
    `tmp_path`.

    Placement is asserted alongside the capture (the plan's WP-4 note): this
    repository stages `test/bin/ocx` and puts `~/.ocx/**` on `PATH` through
    direnv, so a `git` in either would shadow the real one for every other
    acceptance module, for `task verify`, and for the developer's own shell.

    The generated source is asserted directly, because its two load-bearing
    properties are properties of that string: the shebang is `sys.executable`
    (the uv venv interpreter is not on the child's `PATH`), and both paths are
    baked in as literals — C-035's allowlist forwards no `__OCX_TESTING_*`
    variable, so an env-var channel would leave the shim writing nowhere inside
    the very run it exists to observe.

    Those two literal checks are the whole property. A third assertion once sat
    here — `"os.environ" not in source` — and it measured a spelling, not a
    channel: the shim reads `os.environ` legitimately, to record it, and the
    grep passed only because the import was written `from os import environ`
    for its benefit. Rendered with the bare name, a real env-var channel passes
    it. Deleted, along with the import it forced.

    The child runs with an explicit `cwd` under `tmp_path` so `git_dir` is
    deterministic: the pytest process's own cwd may or may not be a repository
    checkout, and `git_dir_exists` would then read differently per developer.
    """
    home = _home(tmp_path)
    shim = install_git_shim(tmp_path)

    assert shim.executable is not None
    assert shim.executable.name == SHIM_NAME
    assert shim.record_path.name == RECORD_NAME
    assert shim.variant is GitShimVariant.DELEGATE
    assert shim.executable.resolve().is_relative_to(tmp_path.resolve())
    for denied in DENIED_SHIM_ROOTS:
        assert not shim.executable.resolve().is_relative_to(denied.resolve()), (
            f"the shim landed under {denied}, shadowing git for the whole tree"
        )

    source = render_shim_source(
        record_path=shim.record_path, real_git=shim.real_git, version_override=None
    )
    assert source.splitlines()[0] == f"#!{sys.executable}"
    assert str(shim.record_path) in source
    assert str(shim.real_git) in source

    work = tmp_path / "delegated"
    result = subprocess.run(
        ["git", "init", str(work)],
        cwd=str(tmp_path.resolve()),
        env=_shim_env(shim, home, OCX_SHIM_MARKER="sentinel"),
        capture_output=True,
        text=True,
        timeout=HTTP_TIMEOUT_SECONDS,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert (work / ".git").is_dir(), "the real git never ran; the shim swallowed the call"

    invocations = shim.invocations()
    assert len(invocations) == 1, f"expected one recorded invocation, got {invocations!r}"
    recorded = invocations[0]
    assert Path(recorded.argv[0]) == shim.executable.resolve(), (
        "argv slot zero does not name the shim that ran; it is the only field "
        "that says WHICH binary was invoked, and nothing else reads it"
    )
    assert list(recorded.argv)[-2:] == ["init", str(work)]
    assert recorded.env["OCX_SHIM_MARKER"] == "sentinel"
    assert recorded.env["PATH"] == shim.child_path()
    assert recorded.returncode == 0
    assert recorded.cwd == str(tmp_path.resolve())

    # The negative half of the `git_dir_exists` pair: this invocation carried no
    # `-C`, so the shim resolved the git directory from a cwd that is not a
    # repository, snapshotted nothing, and SAYS so. The positive half is
    # `::test_shim_records_the_tempdir_mode_before_it_is_removed`. Without both,
    # a WP-16/17 loop asserting "the secret is in no snapshot value" is vacuous
    # on any run whose invocations all snapshotted nothing.
    assert recorded.git_dir == str(tmp_path.resolve())
    assert recorded.git_dir_exists is False
    assert dict(recorded.snapshot) == {}


def test_shim_records_the_tempdir_mode_before_it_is_removed(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The `-C` directory's mode is observed per invocation and outlives the
    directory.

    Everything ocx writes into its own workspace lives inside a tempdir it
    removes on every path that unwinds (C-033), so by the time a test looks the
    evidence is gone. The record is read here only AFTER the directory has been
    deleted, and the mode is changed between two invocations so a cached or
    constant answer cannot pass.

    It is also the POSITIVE half of the `git_dir_exists` pair: the first
    invocation runs against a directory that is not yet a repository, the second
    against the `.git` the first created, and the snapshot follows. The negative
    half is `::test_shim_records_argv_and_env_then_delegates`.
    """
    home = _home(tmp_path)
    shim = install_git_shim(tmp_path)
    work = tmp_path / "workspace"
    work.mkdir(mode=0o700)

    def run_status() -> None:
        subprocess.run(
            ["git", "-C", str(work), "init"],
            env=_shim_env(shim, home),
            capture_output=True,
            text=True,
            timeout=HTTP_TIMEOUT_SECONDS,
            check=False,
        )

    run_status()
    work.chmod(0o750)
    run_status()
    shutil.rmtree(work)
    assert not work.exists()

    invocations = shim.invocations()
    assert len(invocations) == 2, f"expected two invocations, got {invocations!r}"
    assert [inv.dash_c_dir for inv in invocations] == [str(work), str(work)]
    assert invocations[0].dir_mode is not None
    assert invocations[1].dir_mode is not None
    assert stat.S_IMODE(invocations[0].dir_mode) == 0o700
    assert stat.S_IMODE(invocations[1].dir_mode) == 0o750, (
        "both invocations reported the same mode: the shim is not reading it per "
        "invocation, so C-033's 0700 assertion would hold against a constant"
    )
    assert invocations[1].git_dir == str(work / ".git"), (
        "the shim did not resolve the worktree's git directory; a snapshot taken "
        "elsewhere is empty for a reason no assertion would mention"
    )
    assert invocations[1].git_dir_exists is True
    assert "config" in invocations[1].snapshot, (
        "the git config was not snapshotted; S-030's first two surfaces are "
        "unobservable once the directory is gone"
    )
    assert invocations[1].snapshot["config"], "the snapshotted config is empty"


def test_shim_preserves_exit_code_and_stderr_bytes(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """Wrapping is transparent: same exit code, byte-identical stderr.

    Asserted against the real `git` run directly under the same environment,
    because "the shim returned 128" is satisfied by a shim that returns 128 for
    everything. An `execv` shim would pass this and fail every post-run snapshot;
    a shim that reformats stderr would pass every snapshot and corrupt C-044's
    phrase matching.
    """
    home = _home(tmp_path)
    shim = install_git_shim(tmp_path)
    assert shim.real_git is not None
    not_a_repo = tmp_path / "not-a-repo"
    not_a_repo.mkdir()

    argv = ["-C", str(not_a_repo), "rev-parse", "--verify", "HEAD"]
    baseline_env = {
        "PATH": os.environ.get("PATH", ""),
        "HOME": str(home.path),
        "GIT_CONFIG_NOSYSTEM": "1",
        "LC_ALL": "C",
        "LANGUAGE": "",
    }
    direct = subprocess.run(
        [str(shim.real_git), *argv],
        env=baseline_env,
        capture_output=True,
        timeout=HTTP_TIMEOUT_SECONDS,
        check=False,
    )
    through = subprocess.run(
        ["git", *argv],
        env=_shim_env(shim, home),
        capture_output=True,
        timeout=HTTP_TIMEOUT_SECONDS,
        check=False,
    )

    assert direct.returncode != 0, (
        "the control invocation succeeded; a zero exit code cannot show that a "
        "non-zero one was preserved"
    )
    assert through.returncode == direct.returncode
    assert through.stderr == direct.stderr, (
        "stderr was altered in transit: WP-12's classifier matches these bytes"
    )

    invocations = shim.invocations()
    assert len(invocations) == 1
    assert invocations[0].returncode == direct.returncode


def test_shim_does_not_recurse_into_itself(fake_forge: FakeForge, tmp_path: Path) -> None:
    """The delegate is resolved before the shim's directory reaches `PATH`.

    A shim resolving `git` off the child's `PATH` finds itself and recurses until
    the process dies. One recorded invocation for one call is the whole proof.
    """
    home = _home(tmp_path)
    real_git = resolve_real_git()
    shim = install_git_shim(tmp_path)

    assert shim.real_git is not None
    assert shim.real_git.resolve() == real_git.resolve()
    assert shim.executable is not None
    assert shim.real_git.resolve() != shim.executable.resolve()

    result = subprocess.run(
        ["git", "--version"],
        env=_shim_env(shim, home),
        capture_output=True,
        text=True,
        timeout=HTTP_TIMEOUT_SECONDS,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.startswith("git version ")
    assert len(shim.invocations()) == 1, (
        "one call produced more than one recorded invocation: the shim resolved "
        "itself off PATH"
    )


def test_shim_refuses_placement_outside_tmp_path(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """Both placement checks hold, and neither writes anything before refusing.

    "There is no `git` in `test/bin/`" cannot be turned red without writing one,
    so the installer validates its own target instead. The two checks are not
    redundant: the escaping `subdirectory` defeats a string prefix compare, and
    the denied-root check is the half that still holds when a caller passes the
    wrong root entirely.
    """
    good = install_git_shim(tmp_path)
    assert good.executable is not None
    assert good.executable.resolve().is_relative_to(tmp_path.resolve())

    with pytest.raises(ShimPlacementError):
        install_git_shim(tmp_path, subdirectory="../../../ocx-shim-escape")
    assert not (tmp_path / ".." / ".." / ".." / "ocx-shim-escape").exists()

    probe = "ocx-shim-placement-probe"
    for denied in DENIED_SHIM_ROOTS:
        landed = denied / probe
        try:
            with pytest.raises(ShimPlacementError):
                install_git_shim(denied, subdirectory=probe)
            assert not landed.exists(), (
                f"a shim was written under {denied} before the check refused it; "
                "that directory is on PATH for the whole tree"
            )
        finally:
            if landed.exists():  # pragma: no cover - only on a failing implementation
                shutil.rmtree(landed, ignore_errors=True)


def test_git_below_the_floor_and_at_the_floor(
    fake_forge: FakeForge, tmp_path: Path
) -> None:
    """Both version-spoofing variants answer the string they claim, and delegate
    everything else.

    C-075 requires the gate's ACCEPT side to be proved at the exact boundary
    release: a gate that refuses everything passes every refusal test, so
    `VERSION_2_31_0` is not redundant with `DELEGATE`. Both variants existed with
    no consumer at all (DX-18); this is the fixture-level half, and the
    end-to-end ocx behaviour is WP-17's.

    The host's own version is asserted to differ from both spoofs — otherwise a
    shim that silently delegated `--version` would pass.
    """
    home = _home(tmp_path)
    host_version = run_git("--version", home=home.path).stdout.strip()

    for index, variant in enumerate(
        (GitShimVariant.VERSION_2_30_9, GitShimVariant.VERSION_2_31_0)
    ):
        expected = VERSION_OVERRIDES[variant]
        assert expected is not None
        assert host_version != expected, (
            f"the host already reports {expected!r}; the spoof is indistinguishable "
            "from delegation and proves nothing"
        )

        shim = install_git_shim(
            tmp_path, variant=variant, subdirectory=f"shim-{index}-{variant.value}"
        )
        env = {"PATH": shim.child_path()}
        assert (
            run_git("--version", home=home.path, extra_env=env).stdout.strip() == expected
        )

        work = tmp_path / f"delegated-{index}"
        run_git("init", str(work), home=home.path, extra_env=env)
        assert (work / ".git").is_dir(), (
            f"{variant.value} spoofed --version but stopped delegating: a variant "
            "that answers nothing else cannot stand in for a real git"
        )
