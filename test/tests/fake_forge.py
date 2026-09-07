# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Stdlib fake forge server for `ocx package announce` acceptance tests.

Serves **both** forge surfaces from one process over one git object graph:
GitHub under `/repos/...` (below) and GitLab under `/projects/...`
(`fake_gitlab.py`). One graph is deliberate — it lets every announce scenario run
against both clients and assert the same outcome, instead of each client
agreeing with a fixture written for it.

Implements exactly the REST surface `GitHubForge`
(`crates/ocx_lib/src/forge/github.rs`) calls: repo/fork metadata (also used as
the bounded fork-readiness poll target), fork create, the contents API (raw
bytes), git-ref lookup, the git data API (blobs -> trees -> commits -> refs),
and pull-request create/list. A minimal in-memory git object graph
(blobs/trees/commits/refs, keyed by `owner/repo`) is tracked so `commit_files`
followed by a later `get_file_contents` round-trips correctly — required by
the C6 unchanged short-circuit and the C4 branch-head-accumulation semantics.

Per-test instance (the `fake_forge` fixture in `test/conftest.py`), bound to
an ephemeral loopback port, zero real network. Pointed at via
`__OCX_TESTING_FORGE_BASE_URL`. Mirrors the `_ForgeApi` pattern in the
grimoire donor (`research_grimoire_announce_port.md`), minus the git-transport
layer OCX does not use (REST-only, design register S1).
"""
from __future__ import annotations

import base64
import hashlib
import http.server
import json
import re
import threading
import urllib.parse
from collections.abc import Sequence
from email.message import Message as EmailMessage
from typing import Any, NamedTuple

# Plain imports of two SIBLING modules, resolved off `sys.path`. Nothing puts
# `test/tests/` there except pytest, whose default `prepend` import mode inserts
# a test file's own directory when the directory carries no `__init__.py` —
# which is this tree's convention. `test/pyproject.toml`'s `pythonpath` entry is
# `test/`, not `test/tests/`, so it does not cover these.
#
# **pytest is therefore the only supported loader of this module**, and that is
# true of the whole `test/tests/` tree. It is not weakened by
# `conftest._load_fake_forge_module` loading THIS file by path: that exists to
# avoid the import-mode ambiguity of a session-root conftest importing out of a
# non-package directory, and by the time it runs, collection has already put
# `test/tests/` on the path. Importing `fake_forge` from a bare interpreter
# needs that entry added first.
from fake_gitlab import GitLabRoutes
from git_http_fixture import GitFixtureError, GitHttpRoutes

#: The request headers `FakeForge.record` keeps per REST request, in this order.
#: All three of C-026's outcomes are decided by which of these is present:
#: `JOB-TOKEN` under a job token, `PRIVATE-TOKEN` otherwise, none for an empty
#: credential. `Authorization` is here too because the git half carries the
#: credential that way (C-034) and S-040 asks whether it reached a project it
#: should not have. Lookup is case-insensitive against `http.client.HTTPMessage`,
#: which is what the handler passes.
#:
#: **A closed set of three, deliberately, and not "the whole header map".** The
#: two questions the REST log has to answer — C-026's "which credential header
#: did this read carry" and S-040's "did the credential reach a project it
#: should not have" — are both fully served by these three names, and a full
#: header map is a structure pytest may dump verbatim into a failure report. A
#: `PRIVATE-TOKEN` value printed on a red build is a worse trade than a fourth
#: name someone has to add here first. Widen it when a contract needs a fourth
#: name, not in anticipation. (The git transport is the other side of this
#: trade: `GitRequest.headers` keeps everything, because there the header map IS
#: the observation point and nothing else records the request at all.)
AUTH_HEADERS: Sequence[str] = ("Authorization", "PRIVATE-TOKEN", "JOB-TOKEN")


# A repo/branch path segment (owner, repo names never contain '/').
_SEGMENT = r"[^/]+"

_REPO_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})$")
_CONTENTS_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/contents/(?P<path>.+)$")
_REF_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/ref/heads/(?P<branch>{_SEGMENT})$")
# The update-ref endpoint is the PLURAL `/git/refs/heads/<branch>` (PATCH),
# distinct from the SINGULAR `/git/ref/heads/<branch>` GET above — GitHub's own
# asymmetry. Reusing `_REF_RE` here silently 404s every ref update, so the
# fast-forward-only CAS path (`handle_patch_ref`) is never reached.
_REF_UPDATE_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/refs/heads/(?P<branch>{_SEGMENT})$")
_COMMIT_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/commits/(?P<sha>{_SEGMENT})$")
# `GET /repos/<owner>/<repo>/compare/<base>...<head-owner>:<head-branch>` — the
# ancestry question the C6 ensure-PR gate asks ("is the announce branch AHEAD of
# the upstream base?"). The whole `base...owner:branch` spec is one path segment.
_COMPARE_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/compare/(?P<basehead>{_SEGMENT})$")
_PULLS_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/pulls$")
# `GET /repos/<owner>/<repo>/pulls/<number>` — the SINGLE pull request. Distinct
# from `_PULLS_RE` above on purpose: `mergeable` is absent from every entry the
# list endpoint returns, so the mergeability detector has to come here.
_PULL_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/pulls/(?P<number>\d+)$")
_FORKS_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/forks$")
_BLOBS_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/blobs$")
_TREES_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/trees$")
_COMMITS_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/commits$")
_REFS_CREATE_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/refs$")
# `POST /repos/<owner>/<repo>/merge-upstream` — GitHub's "Sync fork". The client
# calls it before every fork announce and once before each git-data replay, so a
# fake without this route answers 404 and the client warns on every run.
_MERGE_UPSTREAM_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/merge-upstream$")
# `GET /users/<login>` — GitHub's user lookup (C-024). Distinct from GitLab's
# `GET /users?username=<login>` (`fake_gitlab.py`), which is the bare `/users`
# path with a query, so the two never collide.
_USER_BY_LOGIN_RE = re.compile(rf"^/users/(?P<login>{_SEGMENT})$")


class ForgeUser(NamedTuple):
    """An account both user APIs answer from.

    `login` is the CANONICAL spelling the forge holds, which is what C-048's
    confirm step replaces a caller's `AliCe` with. `bot` carries the forge's own
    assertion (GitHub's `type == "Bot"`, GitLab's `bot` field) — never a login
    heuristic, so a test cannot accidentally prove the weak form (C-049) while
    believing it proved the strong one.

    **`NamedTuple`, and this module can hold nothing else.** `test/conftest.py`
    loads this file with `spec_from_file_location` + `exec_module` and never
    registers it in `sys.modules`; under `from __future__ import annotations`,
    `dataclasses._process_class` resolves string annotations through
    `sys.modules.get(cls.__module__).__dict__`, which is then `None`. A
    `@dataclass` here — with or without `slots` — raises `AttributeError:
    'NoneType' object has no attribute '__dict__'` at import and takes every
    consumer of the `fake_forge` fixture down with it. `NamedTuple` resolves its
    annotations lazily and is unaffected. Dataclasses belong in
    `git_http_fixture.py` / `fake_gitlab.py`, which are imported normally.
    """

    login: str
    id: int
    bot: bool = False


class _Handler(http.server.BaseHTTPRequestHandler):
    server: FakeForge  # narrows the inherited Any-typed attribute

    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: object) -> None:
        pass  # quiet test output — assertions read `server.requests` instead

    # ── request helpers ──────────────────────────────────────────────────

    def _read_body(self) -> dict[str, Any]:
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            return json.loads(raw) if raw else {}
        except ValueError:
            return {}

    def _reply_json(self, status: int, payload: object) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _reply_raw(self, status: int, body: bytes) -> None:
        self.send_response(status)
        self.send_header("Content-Type", "application/vnd.github.raw+json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _reply_redirect(self, status: int, location: str) -> None:
        self.send_response(status)
        self.send_header("Location", location)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _reply_stub(self, stub: NotImplementedError) -> None:
        """Answer a deferred fixture stub as **501, with its message in the
        body**.

        A `NotImplementedError` raised inside a handler thread is not a message
        to anybody: `socketserver` prints a traceback to the test's stderr and
        drops the connection, so the eventual consumer sees a socket error and
        has to go read the fixture to learn why. 501 puts the sentence the stub
        author wrote where the caller will actually read it, and leaves the
        request in `requests` rather than half-recorded.

        `NotImplementedError` is caught as-is rather than through a private
        subclass: in this file every one of them IS a deferred stub, and a
        second exception type would have to be imported by `fake_gitlab` and
        `git_http_fixture` to be raised there. The stubs all raise before any
        byte of a response is written, which is what makes one reply here safe.

        The stubs raised on the TEST thread — `auth_headers_for`, `seed_user` —
        stay raises: there the exception is the right signal, and there is no
        response to put it in.
        """
        self._reply_json(501, {"message": str(stub)})

    # ── dispatch ──────────────────────────────────────────────────────────

    def do_GET(self) -> None:
        try:
            self._route_get()
        except NotImplementedError as stub:
            self._reply_stub(stub)

    def do_POST(self) -> None:
        try:
            self._route_post()
        except NotImplementedError as stub:
            self._reply_stub(stub)

    def do_PATCH(self) -> None:
        try:
            self._route_patch()
        except NotImplementedError as stub:
            self._reply_stub(stub)

    def _route_get(self) -> None:
        parts = urllib.parse.urlsplit(self.path)
        path = parts.path
        query = urllib.parse.parse_qs(parts.query)

        # Git-transport traffic goes in its OWN log, dispatched BEFORE
        # `record()`. `request_count` is matched exactly against `requests` and
        # has 28 call sites in `test_announce.py`; folding clone/fetch/push
        # requests into it would silently turn S-026's "zero REST writes on
        # every git failure path" into an unfalsifiable assertion. A path is a
        # git route only when its project segment names a repository created
        # through `git_create_project`, so a test that creates none never
        # reaches this branch at all.
        if self.server.git_http_get(self, path, parts.query):
            return

        self.server.record("GET", path, self.path, self.headers)

        if path == "/user":
            self.server.handle_get_authenticated_user(self)
            return

        if self.server.gitlab_get(self, path, query):
            return

        match = _USER_BY_LOGIN_RE.fullmatch(path)
        if match:
            self.server.handle_get_user_by_login(self, match.group("login"))
            return

        match = _REPO_RE.fullmatch(path)
        if match:
            self.server.handle_get_repo(self, match.group("owner"), match.group("repo"))
            return

        match = _CONTENTS_RE.fullmatch(path)
        if match:
            ref = query.get("ref", [None])[0]
            self.server.handle_get_contents(self, match.group("owner"), match.group("repo"), match.group("path"), ref)
            return

        match = _REF_RE.fullmatch(path)
        if match:
            self.server.handle_get_ref(self, match.group("owner"), match.group("repo"), match.group("branch"))
            return

        match = _COMMIT_RE.fullmatch(path)
        if match:
            self.server.handle_get_commit(self, match.group("owner"), match.group("repo"), match.group("sha"))
            return

        match = _COMPARE_RE.fullmatch(path)
        if match:
            self.server.handle_get_compare(self, match.group("owner"), match.group("repo"), match.group("basehead"))
            return

        match = _PULLS_RE.fullmatch(path)
        if match:
            head = query.get("head", [None])[0]
            state = query.get("state", [None])[0]
            self.server.handle_get_pulls(self, match.group("owner"), match.group("repo"), head, state)
            return

        match = _PULL_RE.fullmatch(path)
        if match:
            self.server.handle_get_pull(
                self, match.group("owner"), match.group("repo"), int(match.group("number"))
            )
            return

        self._reply_json(404, {"message": "not found"})

    def _route_post(self) -> None:
        parts = urllib.parse.urlsplit(self.path)
        path = parts.path

        # Before `record()` (see `do_GET`) AND before `_read_body()`: a
        # `git-receive-pack` body is pack data, and `_read_body` would consume it
        # as JSON and hand the CGI child an empty stdin.
        if self.server.git_http_post(self, path, parts.query):
            return

        self.server.record("POST", path, headers=self.headers)
        body = self._read_body()
        self.server.record_body(path, body)

        if self.server.gitlab_post(self, path, body):
            return

        match = _FORKS_RE.fullmatch(path)
        if match:
            self.server.handle_post_fork(self, match.group("owner"), match.group("repo"), body)
            return

        match = _BLOBS_RE.fullmatch(path)
        if match:
            self.server.handle_post_blob(self, match.group("owner"), match.group("repo"), body)
            return

        match = _TREES_RE.fullmatch(path)
        if match:
            self.server.handle_post_tree(self, match.group("owner"), match.group("repo"), body)
            return

        match = _COMMITS_RE.fullmatch(path)
        if match:
            self.server.handle_post_commit(self, match.group("owner"), match.group("repo"), body)
            return

        match = _REFS_CREATE_RE.fullmatch(path)
        if match:
            self.server.handle_post_ref(self, match.group("owner"), match.group("repo"), body)
            return

        match = _PULLS_RE.fullmatch(path)
        if match:
            self.server.handle_post_pull(self, match.group("owner"), match.group("repo"), body)
            return

        if _MERGE_UPSTREAM_RE.fullmatch(path):
            self._reply_json(200, {"merge_type": "fast-forward"})
            return

        self._reply_json(404, {"message": "not found"})

    def _route_patch(self) -> None:
        path = urllib.parse.urlsplit(self.path).path
        self.server.record("PATCH", path, headers=self.headers)
        body = self._read_body()

        match = _REF_UPDATE_RE.fullmatch(path)
        if match:
            self.server.handle_patch_ref(self, match.group("owner"), match.group("repo"), match.group("branch"), body)
            return

        self._reply_json(404, {"message": "not found"})


class FakeForge(GitHttpRoutes, GitLabRoutes, http.server.ThreadingHTTPServer):
    """A per-test fake forge (GitHub, GitLab and git-over-HTTP surfaces), bound
    to an ephemeral loopback port."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.requests: list[tuple[str, str]] = []
        # The same log with the query string kept. `requests` is matched
        # exactly by `request_count`, so it cannot carry one; assertions about
        # WHICH ref a read named (a branch name versus a pinned commit) have
        # nowhere else to look.
        self.raw_requests: list[tuple[str, str]] = []
        # The auth-bearing request headers of each recorded REST request, in
        # `AUTH_HEADERS` order, ALIGNED to `requests` — its own list, never a
        # field on `requests`, which `request_count` exact-matches at 28 sites.
        # This is the only server-side observation point for C-026's header
        # selection: without it "reads carry `JOB-TOKEN`" (S-014) and
        # "the API half keeps its own token" are true in every state of the code.
        # Each name maps to ALL of its values: a repeated header name is a
        # regression worth reporting, not one worth silently keeping half of.
        self.auth_headers: list[dict[str, list[str]]] = []
        self.bodies: list[tuple[str, dict[str, Any]]] = []
        self.token_identity_login = "test-forge-bot"
        # The numeric id `/user` reports. `owners[]` is `login` + `id` on the
        # wire (C-008), so an identity with no id cannot exercise the ladder at
        # all.
        self.token_identity_id = 1001
        # When True `/user` reports `type: "Bot"` (GitHub) and `bot: true`
        # (GitLab) — the forge's OWN assertion, which is the strong form C-049
        # refuses on. Default False: every existing consumer reads this endpoint
        # and none of them expects a bot.
        self.token_identity_is_bot = False
        # When True `/user` answers `403 {"message": "Resource not accessible by
        # integration"}` — "there is no user behind this credential", the ONLY
        # shape GitHub reads as `Ok(None)` (`github.rs::authenticated_login`,
        # DX-42), and on the GitLab surface the same 403 under a job token is
        # `UsersApiUnavailable` (`gitlab.rs::users_api_is_out_of_reach`). One
        # field therefore serves both surfaces.
        #
        # SEPARATE from `users_api_status` on purpose, not as a second spelling
        # of it (DX-71 / hunt K-5). `users_api_status`'s documented scope is all
        # THREE user routes, so arming it also silences `/users/<login>` — and
        # the tests that need "no token identity" (`resolve_author`'s rungs 2
        # and 3, `::test_no_acting_identity_64`) still need the owner lookup to
        # answer. This knob is narrow and named for the contract rather than for
        # a status. Default False: the identity read every existing consumer
        # makes is untouched.
        self.token_identity_absent = False
        # Accounts the two user-lookup routes answer from, keyed by LOWERCASED
        # login so a supplied `AliCe` resolves and is answered with the
        # canonical spelling (C-048). Seed through `seed_user`.
        self.users: dict[str, ForgeUser] = {}
        # When set, `/user`, `/users/<login>` and `/users?username=` all answer
        # this status instead of a body — the credential that may not call the
        # endpoint at all (`UsersApiUnavailable`, C-009/C-027). `None` (default)
        # keeps every existing consumer's identity read working.
        self.users_api_status: int | None = None

        # Repo metadata: "owner/repo" -> {"full_name", "owner", "parent"}.
        self.repos: dict[str, dict[str, Any]] = {}
        # Git object graph, shared across all repos (mirrors a real forge's
        # shared object store between a fork and its upstream).
        self.refs: dict[str, dict[str, str]] = {}  # "owner/repo" -> {branch: commit_sha}
        self.commits: dict[str, dict[str, Any]] = {}  # sha -> {"tree", "parent"}
        self.trees: dict[str, dict[str, str]] = {}  # sha -> {path: blob_sha} (flattened)
        self.blobs: dict[str, bytes] = {}  # sha -> raw bytes
        # commit sha -> {path: the commit that last changed that path}. GitLab's
        # only compare-and-swap is per file, so the fake must track it; GitHub
        # never reads it.
        self.file_last_commit: dict[str, dict[str, str]] = {}
        self._commit_counter = 0

        # Idempotent fork-create tracking: (upstream_full_name, target_owner) -> record.
        self.forks: dict[tuple[str, str | None], dict[str, str]] = {}
        # Open pull requests: (owner, repo) -> {head: {"number", "html_url"}}.
        self.open_prs: dict[tuple[str, str], dict[str, dict[str, Any]]] = {}
        self._pr_counter = 0

        # Scripting knobs (mutate before invoking the CLI):
        # A renamed fork: overrides the create-response `full_name` (S5 X5).
        self.rename_fork_to: str | None = None
        # Forces a fork-create response's `parent.full_name` to mismatch the
        # upstream, exercising the parent-verification guard.
        self.fork_parent_override: str | None = None
        # "owner/repo" (registered, post-create) -> remaining not-ready GETs;
        # -1 means "never ready" (used by the bounded readiness-retry proof).
        self.not_ready: dict[str, int] = {}
        # "owner/repo" repos whose NEXT POST .../git/trees 404s once, then
        # succeeds — the fresh-fork write-race retry (X5).
        self.tree_fail_once: set[str] = set()
        # "owner/repo" repos whose NEXT GET .../git/commits/<sha> 404s once,
        # then succeeds. That GET is the FIRST request of the commit sequence
        # (it reads the base tree), so it is where a brand-new fork's
        # unprovisioned git object store is hit first — the readiness poll asks
        # for repository METADATA, which goes ready earlier (X5).
        self.base_commit_fail_once: set[str] = set()
        # When set, the next contents GET replies with a 302 to this location
        # instead of the real content — proves the no-redirect client does
        # not chase it (X5).
        self.redirect_next_contents: str | None = None
        # When True, the NEXT POST .../pulls replies 500 once (then clears) — a
        # pull-request open that fails after the commit already landed, for the
        # C6-amendment PR-recovery proof (F1).
        self.pull_fail_once: bool = False
        # "owner/repo/branch" -> files to inject as a racing announce's commit
        # (advancing the branch head) before the next PATCH ref-update on that
        # branch, which is then rejected non-fast-forward (422). Models a
        # concurrent announce that advanced the branch between our read and our
        # commit (design register C4 amendment F2). Fires once, then clears.
        self.concurrent_ref_advance: dict[str, dict[str, bytes]] = {}
        # "owner/repo/branch" keys whose `concurrent_ref_advance` entry is
        # RE-ARMED after it fires, so every fast-forward-only ref update on that
        # branch is refused for as long as the key is here (DX-87).
        #
        # A one-shot advance can never reach S-003's exit-75 cell: claim
        # re-reads the winning head and regenerates EXACTLY once, and with the
        # entry popped that single retry always succeeds — so "regenerate once,
        # then 75" has no reachable state and an exit-code assertion on it would
        # be unfalsifiable. A persistent racing writer is what the cell
        # describes, and re-arming models one writer that keeps landing rather
        # than a server that refuses unconditionally: each refusal injects a
        # fresh commit, so the head genuinely moves under every attempt.
        #
        # Additive and empty by default, so every existing consumer of
        # `concurrent_ref_advance` keeps the one-shot behaviour its convergence
        # proofs depend on.
        self.concurrent_ref_advance_persists: set[str] = set()
        # When True, the NEXT compare GET replies 404 — an INDETERMINATE
        # ancestry answer (a ref unresolvable or the compare inaccessible),
        # which the C6 ensure-PR gate must refuse rather than read as "not
        # ahead". Fires once, then clears.
        self.compare_404_once: bool = False
        # When set, the NEXT compare GET replies 200 carrying this `status`
        # value verbatim — used to prove a value the client does not model is
        # refused, not guessed. Fires once, then clears.
        self.compare_status_once: str | None = None
        # "owner/repo" repos whose metadata reports no push permission — the
        # fork-free announce path's up-front push probe must refuse before
        # writing anything. Read by both surfaces.
        self.no_push_access: set[str] = set()

        # ── GitLab surface state (see `fake_gitlab.py`) ───────────────────
        # Project paths carry numeric ids on GitLab; they are assigned lazily
        # and are stable for the life of the server.
        self.gitlab_ids: dict[str, int] = {}
        self._gitlab_id_counter = 0
        # (target_path, source_path, source_branch) -> open merge request.
        self.gitlab_merge_requests: dict[tuple[str, str, str], dict[str, Any]] = {}
        self._gitlab_mr_counter = 0
        # Project path -> reported `import_status`.
        self.gitlab_import_status: dict[str, str] = {}
        # Project path -> remaining reads that report an unfinished import;
        # -1 means "never finishes" (the bounded-wait proof).
        self.gitlab_import_pending: dict[str, int] = {}
        # A renamed fork: overrides the fork-create response path.
        self.gitlab_rename_fork_to: str | None = None
        # Forces a fork's parent to a stranger project, exercising the
        # parent-verification guard.
        self.gitlab_fork_parent_override: str | None = None
        # "project/branch" -> files a racing announce lands first, moving the
        # root's last commit so the next commit's `last_commit_id` is stale.
        self.gitlab_concurrent_advance: dict[str, dict[str, bytes]] = {}
        # When True the NEXT compare reports `compare_timeout`, an answer the
        # client must refuse rather than read as "no commits".
        self.gitlab_compare_timeout_once: bool = False
        # When True the NEXT compare replies 200 with NO `commits` key — the
        # shape a proxy or an API change can produce, and one the client must
        # refuse rather than count as zero commits.
        self.gitlab_compare_malformed_once: bool = False
        # When True the NEXT merge-request create replies 500 once.
        self.gitlab_merge_request_fail_once: bool = False
        # Project path -> the `ci_push_repository_for_job_token_allowed` value
        # the project body reports. THREE states, not two (C-029): `True`
        # passes, `False` refuses at 86 before any push, and a project ABSENT
        # from this dict emits **no field at all** — the GitLab < 18.4 / hidden
        # case that is the only one yielding `unknown`-and-proceed. Absence is
        # the default, so every existing consumer sees the body it sees today.
        self.gitlab_job_token_push_allowed: dict[str, bool] = {}
        # Target project path -> the source project paths its job-token
        # allowlist contains. An absent key is an EMPTY allowlist (a miss), not
        # an unreadable one: C-029 gives the two different exit codes, and
        # unreadable is what `gl_job_token_refuses` answers instead.
        self.gitlab_job_token_allowlist: dict[str, list[str]] = {}
        # Target project path -> the GROUP paths its job-token allowlist
        # contains. GitLab keeps two independent lists and a group entry admits
        # every project under it at any depth (#430); this is the second one.
        # Absent key is an EMPTY list, so an existing row that seeds only
        # `gitlab_job_token_allowlist` keeps meaning exactly what it means
        # today — neither list admits, which is still a miss.
        self.gitlab_job_token_groups_allowlist: dict[str, list[str]] = {}
        # No `..._unreadable` knob for either list: the 403 one would arm is
        # already what `gl_job_token_refuses` answers a bare job token, which is
        # the posture that meets it in production. A knob no row arms is a
        # branch no row covers.

        self.git_http_init()

        super().__init__(("127.0.0.1", 0), _Handler)

    def server_close(self) -> None:
        """Close the socket, then drop the git scratch project root.

        Implemented rather than stubbed: `test/conftest.py`'s `fake_forge`
        fixture calls this on every teardown for all six existing consumer
        modules, so a raising body here would fail them all.
        """
        super().server_close()
        self.git_http_cleanup()

    @property
    def base_url(self) -> str:
        host, port = self.server_address[:2]
        return f"http://{host}:{port}"

    def record(
        self,
        method: str,
        path: str,
        raw: str | None = None,
        headers: EmailMessage | None = None,
    ) -> None:
        """Log one REST request. `headers` is the request's whole header map;
        only the `AUTH_HEADERS` names are kept, in `auth_headers`.

        The capture is implemented rather than stubbed because every existing
        consumer traverses this method, and an accessor over a list nothing
        appends to is a green that can never go red.

        Each kept name maps to **all** of its values, through `get_all`. A
        request may legitimately carry a header name twice, and `headers[name]`
        answers only the FIRST — the opposite collapse to the one a dict
        comprehension over `.items()` makes, which is what the git transport
        used to do. Two logs disagreeing about which duplicate survived is worse
        than either collapse alone: the same regression then reads differently
        depending on which log an assertion happened to consult.
        """
        captured = (
            {
                name: list(values)
                for name in AUTH_HEADERS
                if (values := headers.get_all(name))
            }
            if headers is not None
            else {}
        )
        with self.lock:
            self.requests.append((method, path))
            self.raw_requests.append((method, raw if raw is not None else path))
            self.auth_headers.append(captured)

    def record_body(self, path: str, body: dict[str, Any]) -> None:
        with self.lock:
            self.bodies.append((path, body))

    def request_count(self, method: str, path: str) -> int:
        with self.lock:
            return sum(1 for m, p in self.requests if m == method and p == path)

    def auth_headers_for(self, method: str, path: str) -> list[dict[str, list[str]]]:
        """The auth headers of every recorded `(method, path)` request, in order.

        One entry per matching request, `{}` when a request carried none — an
        empty dict and a missing entry are different answers, and C-026's
        "empty credential sends no header" arm needs the first, not the second.
        An accessor that skipped headerless requests would answer one element
        where two were made, and the arm asserting "this read carried nothing"
        would then be asserting about the wrong read.

        `path` is matched against `requests`, which carries no query string, so
        two reads of one endpoint under different queries are two entries here.
        Values are lists for the reason `record` gives.
        """
        with self.lock:
            return [
                {name: list(values) for name, values in captured.items()}
                for (seen_method, seen_path), captured in zip(
                    self.requests, self.auth_headers, strict=True
                )
                if seen_method == method and seen_path == path
            ]

    def read_file(self, owner: str, repo: str, path: str, *, branch: str = "main") -> bytes | None:
        """Test-assertion helper: reads a committed file directly from the
        in-memory git graph, bypassing HTTP — the only way to inspect what a
        `--fork` run committed onto its own branch, since `--out` mode never
        surfaces it (it reads/writes independently of the forge commit
        history) and the real forge has no "diff" endpoint to poll."""
        full = f"{owner}/{repo}"
        with self.lock:
            commit_sha = self.refs.get(full, {}).get(branch)
            if commit_sha is None:
                return None
            tree = self.trees[self.commits[commit_sha]["tree"]]
            blob_sha = tree.get(path)
            if blob_sha is None:
                return None
            return self.blobs[blob_sha]

    # ── test setup ────────────────────────────────────────────────────────

    def seed_root(self, owner: str, repo: str, path: str, root: dict[str, Any], *, branch: str = "main") -> None:
        """Seeds a single committed file (typically a package root) onto
        `branch`, extending the branch's existing tree when present."""
        self.seed_files(owner, repo, {path: json.dumps(root).encode()}, branch=branch)

    def seed_files(self, owner: str, repo: str, files: dict[str, bytes], *, branch: str = "main") -> None:
        """Commit `files` onto `branch`, advancing its head.

        Refuses on a git-transport project — see `_reject_git_project_locked`.
        The guard sits HERE, on the test-facing entry point, and not on
        `_seed_files_locked`: two route handlers (`handle_patch_ref`'s
        `concurrent_ref_advance` branch and `fake_gitlab`'s commit route) call
        that helper to inject a racing writer, and a guard below them would raise
        `GitFixtureError` inside a handler thread — a traceback on the test's
        stderr and a dropped connection, not a status. `seed_root` routes through
        here; `seed_branch_at` keeps its own call.
        """
        with self.lock:
            self._reject_git_project_locked(f"{owner}/{repo}")
            self._seed_files_locked(owner, repo, files, branch)

    def seed_branch_at(
        self,
        owner: str,
        repo: str,
        branch: str,
        *,
        source_owner: str,
        source_repo: str,
        source_branch: str = "main",
    ) -> None:
        """Point `owner/repo@branch` at another repo's branch head verbatim,
        creating no commit — the "the announce branch exists but is NOT ahead of
        the upstream base" state (a merged-and-not-deleted branch reads this way
        too). Distinct from `seed_files`, which always advances the head.

        Refuses on a git-transport project for the same reason `seed_files`
        does: it writes a ref the bare repository does not have. It mints no sha
        of its own, so the guard is a SEPARATE call rather than something one
        seeder could cover for the other — the two are the only test-facing
        writers of `refs`, and a rule enforced at one of them is a rule the other
        silently breaks."""
        source_full = f"{source_owner}/{source_repo}"
        full = f"{owner}/{repo}"
        with self.lock:
            self._reject_git_project_locked(full)
            self.refs.setdefault(full, {})[branch] = self.refs[source_full][source_branch]
            self.repos.setdefault(full, {"full_name": full, "owner": owner, "parent": source_full})

    def seed_user(self, login: str, user_id: int, *, bot: bool = False) -> ForgeUser:
        """Register an account both user-lookup routes answer from.

        `login` is stored as the CANONICAL spelling and matched
        case-insensitively, so seeding `alice` and resolving `AliCe` returns
        `alice` — C-048's confirm step replaces the caller's spelling with the
        server's, and a case-sensitive store could never show that. The KEY is
        lowercased and the VALUE keeps the spelling, which is the whole
        mechanism: a store that keyed on the spelling as given would 404 the
        caller's `AliCe`, and one that echoed the caller's spelling back would
        turn C-048's case-fold into an echo.
        """
        user = ForgeUser(login=login, id=user_id, bot=bot)
        with self.lock:
            self.users[login.lower()] = user
        return user

    def seed_token_identity(self, *, bot: bool = False) -> ForgeUser:
        """Register the CREDENTIAL's own identity as a lookup-able account.

        `users` starts empty while `/user` always answers
        `token_identity_login` / `token_identity_id` (DX-74). So the owner
        ladder's token rung resolves `test-forge-bot`, `confirm_with_forge` then
        asks `GET /users/test-forge-bot`, gets the 404, and the run exits **79**
        (`OwnerUnknown`) — for a fixture reason, in a test that reads as if it
        had exercised the rung. Every `ocx package claim` run without `--owner`
        needs this seeding first.

        It exists as its own call rather than as `seed_user("test-forge-bot",
        1001)` at each site because the login and the id are the SERVER's, and a
        test that restates them drifts the moment either default moves — the
        same reason `seed_user` keys on the canonical spelling instead of the
        caller's.

        `bot=True` seeds the identity as a bot on the LOOKUP side only, which is
        `confirm_with_forge`'s strong guard; `token_identity_is_bot` is the
        detected-side twin. They are two guards at two lines, so
        `::test_owner_bot_refused_64` arms them in SEPARATE rows and never both
        on one run: a run carrying both is refused by whichever fires first, so
        deleting either guard leaves the other still refusing and the row green
        with its own mutation applied.

        Consumed by every no-`--owner` row of `test_package_claim.py`, and
        pinned by `::test_owner_unknown_79`, whose positive control is the same
        run with the seeding present.

        It routes through `seed_user` rather than writing `users` itself, so the
        canonical-spelling rule has exactly one implementation: a second writer
        keying on the caller's spelling would make `seed_user("AliCe", ...)` and
        this call disagree about what the store holds.
        """
        return self.seed_user(self.token_identity_login, self.token_identity_id, bot=bot)

    def close_pull_request(self, owner: str, repo: str, head: str) -> None:
        """Drop the open pull request whose head is `head` (`"<owner>:<branch>"`),
        leaving its branch in place. Models both halves of the trap #228 is about:
        a merged pull request and a closed-unmerged one look identical from the
        branch's side, because the branch is per package and outlives either."""
        with self.lock:
            self.open_prs.get((owner, repo), {}).pop(head, None)

    def commit_parent(self, owner: str, repo: str, branch: str) -> str | None:
        """The parent sha of `branch`'s head commit — what a committed announce
        was actually built ON, as opposed to what it contains."""
        with self.lock:
            head = self.refs.get(f"{owner}/{repo}", {}).get(branch)
            return None if head is None else self.commits[head]["parent"]

    def branch_head(self, owner: str, repo: str, branch: str) -> str | None:
        """The head sha of `branch`, or `None` when the ref does not exist."""
        with self.lock:
            return self.refs.get(f"{owner}/{repo}", {}).get(branch)

    def _reject_git_project_locked(self, full: str) -> None:
        """Refuse REST-side seeding on a project that also owns a bare repository
        (caller holds `self.lock`).

        The shas the REST seeders mint are synthetic; a git-registered project's
        shas are real, and DX-5's whole premise is that a REST read and a
        `--force-with-lease` answer the same one. Mixing the two writers makes
        them disagree silently, and every C-042 / S-024 / S-025 assertion then
        measures the fixture. Failing loudly at the seam is what keeps that from
        needing per-test discipline — use `git_seed_files` on a git project.

        Called from both test-facing **synthetic-sha** writers of `refs`,
        `seed_files` (and so `seed_root`) and `seed_branch_at` — at the public
        entry points, never from `_seed_files_locked`. The route handlers reach
        that helper to inject a racing writer (`handle_patch_ref`'s
        `concurrent_ref_advance` branch, and `fake_gitlab`'s commit route), and
        raising inside a handler thread is a traceback and a dropped connection
        rather than a status. Those handlers are also not what this guard is
        about: they write `refs` as the surface under test, which is the thing
        the tests exist to observe.

        `git_import_ref` is the deliberate third test-facing writer of `refs`
        and is **exempt** — it carries the bare repository's real sha, which is
        the one thing this guard exists to keep `refs` agreeing with. Calling it
        from there would make every git-registered project unseedable and take
        the whole transport with it.
        """
        if full in self.git_http_projects:
            raise GitFixtureError(
                f"{full} is a git-transport project: seed it with git_seed_files, "
                "not seed_files/seed_root/seed_branch_at — a synthetic sha here would "
                "disagree with the bare repository's real one"
            )

    def _seed_files_locked(self, owner: str, repo: str, files: dict[str, bytes], branch: str) -> None:
        """Commit `files` onto `branch`, advancing its head (caller holds `self.lock`).

        Unguarded, deliberately: the route handlers that inject a racing writer
        call this, and `_reject_git_project_locked` sits on `seed_files` above
        it. A test seeds through `seed_files` / `seed_root`.
        """
        full = f"{owner}/{repo}"
        parent_sha = self.refs.get(full, {}).get(branch)
        base_tree = dict(self.trees[self.commits[parent_sha]["tree"]]) if parent_sha else {}
        for path, content in files.items():
            base_tree[path] = self._store_blob_locked(content)
        tree_sha = self._store_tree_locked(base_tree)
        commit_sha = self._store_commit_locked(tree_sha, parent_sha)
        self.refs.setdefault(full, {})[branch] = commit_sha
        self.repos.setdefault(full, {"full_name": full, "owner": owner, "parent": None})

    # ── internal object-store primitives (caller holds `self.lock`) ────────

    def _store_blob_locked(self, content: bytes) -> str:
        sha = hashlib.sha1(b"blob:" + content).hexdigest()
        self.blobs[sha] = content
        return sha

    def _store_tree_locked(self, flat: dict[str, str]) -> str:
        key = json.dumps(flat, sort_keys=True).encode()
        sha = hashlib.sha1(b"tree:" + key).hexdigest()
        self.trees[sha] = dict(flat)
        return sha

    def _store_commit_locked(self, tree_sha: str, parent_sha: str | None) -> str:
        """Register a commit under a freshly minted SYNTHETIC sha."""
        self._commit_counter += 1
        key = f"commit:{tree_sha}:{parent_sha}:{self._commit_counter}".encode()
        return self._record_commit_locked(hashlib.sha1(key).hexdigest(), tree_sha, parent_sha)

    def _record_commit_locked(self, sha: str, tree_sha: str, parent_sha: str | None) -> str:
        """Register a commit and its per-path provenance under a CALLER-CHOSEN sha.

        Split out of `_store_commit_locked` so `git_import_ref` can register a
        bare repository's **real** sha under the same provenance rule instead of
        restating it. That rule is GitLab's only compare-and-swap oracle
        (`fake_gitlab.py::gl_post_commit`), and this file's own comment below
        says getting it wrong makes the CAS either never fire or always fire —
        which is exactly why it must have one writer, not two.
        """
        self.commits[sha] = {"tree": tree_sha, "parent": parent_sha}
        # Per-path provenance: a path whose blob is unchanged from the parent
        # keeps the parent's answer, everything else was last changed here. This
        # is what GitLab's `last_commit_id` means, and getting it wrong would
        # make the compare-and-swap either never fire or always fire.
        tree = self.trees.get(tree_sha, {})
        parent_tree = self.trees.get(self.commits.get(parent_sha, {}).get("tree", ""), {}) if parent_sha else {}
        inherited = self.file_last_commit.get(parent_sha or "", {})
        self.file_last_commit[sha] = {
            path: inherited[path] if parent_tree.get(path) == blob and path in inherited else sha
            for path, blob in tree.items()
        }
        return sha

    # ── route handlers ───────────────────────────────────────────────────

    def handle_get_authenticated_user(self, handler: _Handler) -> None:
        """`GET /user` -> the credential's own identity, on both surfaces.

        One identity endpoint, two field names: GitHub reads `login` and
        `type`, GitLab reads `username` and `bot`. Serving all four keeps the
        two surfaces on one account rather than inventing a second test
        identity.

        **All three arms are now live**, because WP-16's rows arm them. The
        default 200 body is unchanged byte for byte: `token_identity_is_bot`
        defaults False, so `type` is `"User"` and `bot` is `false` exactly as
        before, and both new fields default False too. The 200 body is on the
        path every existing announce module traverses, so a raising body there
        would fail all six — the same reason C-017 gives for `ForgeKind::client`.

        `id`, `type` and `bot` DO appear in the default body: they are
        unconditional projections of declared state, and both clients read one
        named field off a `serde_json::Value` (`github.rs::authenticated_login`,
        `gitlab.rs::authenticated_username`) with no `deny_unknown_fields`
        anywhere under `crates/ocx_lib/src/forge/`, so the extra keys are inert.

        **The consumers, one per arm.** `users_api_status` here is
        `test_package_claim.py::test_owner_asserted_needs_a_gitlab_job_token`'s
        GitLab row (DX-72: `asserted` is unreachable on GitHub, so every test
        arming it is a GitLab test); `token_identity_is_bot` is
        `::test_owner_bot_refused_64`'s detected row, the only one that reaches
        `seed_logins`' strong guard (`claim/owners.rs`) rather than the
        login-shape guard; `token_identity_absent` is
        `::test_no_acting_identity_64` and
        `::test_author_is_null_without_a_token_identity_or_a_ci_pair`, which is
        `author`'s null rung AND the observation that this knob really is
        narrower than `users_api_status` — it passes `--owner`, so the owner
        lookup must still answer for that row to succeed at all.

        The two 403s are NOT two spellings of one state. `token_identity_absent`
        is narrow — this route only, with the sentence GitHub reads as "no user
        behind this credential" — while `users_api_status` is the credential that
        may not call the users API at all, and its scope is every user route the
        client can express it on. A test needing the second without the first
        exists (`asserted`); a test needing the first without the second exists
        (`::test_author_is_null_without_a_token_identity_or_a_ci_pair`, whose
        owner lookup must still answer — which is what makes the narrowness
        observable rather than only asserted here).
        """
        if self.token_identity_absent:
            # Checked FIRST, and answering the exact sentence rather than a bare
            # 403: GitHub reads `Ok(None)` only from a 403 whose body carries
            # this message, so a generic body would turn "no user behind this
            # credential" into `Err(Status)` and move the run off the rung the
            # test is aiming at.
            handler._reply_json(403, {"message": "Resource not accessible by integration"})
            return
        if self.users_api_status is not None:
            handler._reply_json(
                self.users_api_status,
                {"message": "the users API is not readable with this credential"},
            )
            return
        handler._reply_json(
            200,
            {
                "login": self.token_identity_login,
                "username": self.token_identity_login,
                "id": self.token_identity_id,
                # The forge's OWN bot assertion, in both spellings: GitHub reads
                # `type`, GitLab reads `bot`. Both move together or the same
                # fixture state would mean "bot" on one surface and "human" on
                # the other.
                "type": "Bot" if self.token_identity_is_bot else "User",
                "bot": self.token_identity_is_bot,
            },
        )

    def handle_get_user_by_login(self, handler: _Handler, login: str) -> None:
        """`GET /users/<login>` -> GitHub's user lookup (C-024).

        404 when the forge has no such account — the client reads that as
        `Ok(None)`, and `OwnerUnknown` (79) is decided above it, so answering
        anything else here would move the decision into the fixture.

        The lookup is a **transformation**, which is why it ships here: the
        caller's spelling is case-folded to find the account and the CANONICAL
        spelling is answered, so `GET /users/AliCe` returns `login == "alice"`.
        That is C-048's confirm step. A case-sensitive store 404s it; a store
        echoing the caller's spelling answers `AliCe` and turns the confirm into
        an echo.

        The `users_api_status` arm stays deferred, for the reason
        `handle_get_authenticated_user` gives about its own two: it is a knob
        echo whose meaning is the Rust client's reaction, no test in this tree
        arms it, and its red state is unreachable until WP-9's consumer exists.
        It reaches the caller as a 501 carrying this sentence (`_reply_stub`).

        **This arm stays deferred, and now for a contract reason rather than a
        scheduling one.** DX-72: `UsersApiUnavailable` is unreachable on GitHub.
        `github.rs::resolve_user` maps 404 to `Ok(None)` and every other
        non-success to `Err(Status)`, so no status this knob could answer
        produces the error the arm is named for — only GitLab does, through
        `gl_get_users`, and only under `api_is_job_token`. Arming it would ship a
        knob whose documented meaning the client cannot express.

        It is therefore also the target of `test_git_http_fixture.py::
        test_a_deferred_stub_answers_501_with_its_message`, which proves the
        deferred-stub MECHANISM and needs one live stub to point at (DX-76).
        WP-16 armed `/user`'s and `gl_get_users`' arms, which is why that probe
        moved here.

        The 404-on-unknown arm below must survive unchanged — `OwnerUnknown`
        (79) is decided above the client, and any other status for a missing
        account turns it into a `ForgeError`.
        """
        if self.users_api_status is not None:
            raise NotImplementedError(
                "handle_get_user_by_login owes C-009/C-027's UsersApiUnavailable status"
            )
        with self.lock:
            user = self.users.get(urllib.parse.unquote(login).lower())
        if user is None:
            handler._reply_json(404, {"message": "not found"})
            return
        handler._reply_json(
            200,
            {
                "login": user.login,
                "username": user.login,
                "id": user.id,
                "type": "Bot" if user.bot else "User",
                "bot": user.bot,
            },
        )

    def handle_get_repo(self, handler: _Handler, owner: str, repo: str) -> None:
        full = f"{owner}/{repo}"
        ready = True
        with self.lock:
            record = self.repos.get(full)
            if record is not None:
                remaining = self.not_ready.get(full, 0)
                if remaining != 0:
                    ready = False
                    if remaining > 0:
                        self.not_ready[full] = remaining - 1
        if record is None:
            handler._reply_json(404, {"message": "not found"})
            return
        if not ready:
            handler._reply_json(404, {"message": "not ready"})
            return
        handler._reply_json(200, self._repo_body(record))

    def handle_get_contents(self, handler: _Handler, owner: str, repo: str, path: str, ref: str | None) -> None:
        if self.redirect_next_contents is not None:
            location = self.redirect_next_contents
            self.redirect_next_contents = None
            handler._reply_redirect(302, location)
            return
        full = f"{owner}/{repo}"
        ref = ref or "main"
        with self.lock:
            # GitHub resolves `ref` as a branch/tag name OR a commit SHA. The
            # C4-F2 retry re-reads the root at the head SHA (not a branch name),
            # so fall back to resolving a bare commit SHA.
            commit_sha = self.refs.get(full, {}).get(ref)
            if commit_sha is None and ref in self.commits:
                commit_sha = ref
            if commit_sha is None:
                handler._reply_json(404, {"message": "not found"})
                return
            tree = self.trees[self.commits[commit_sha]["tree"]]
            blob_sha = tree.get(path)
            if blob_sha is None:
                handler._reply_json(404, {"message": "not found"})
                return
            content = self.blobs[blob_sha]
        handler._reply_raw(200, content)

    def handle_get_ref(self, handler: _Handler, owner: str, repo: str, branch: str) -> None:
        full = f"{owner}/{repo}"
        with self.lock:
            sha = self.refs.get(full, {}).get(branch)
        if sha is None:
            handler._reply_json(404, {"message": "not found"})
            return
        handler._reply_json(200, {"object": {"sha": sha}})

    def handle_get_commit(self, handler: _Handler, owner: str, repo: str, sha: str) -> None:
        full = f"{owner}/{repo}"
        with self.lock:
            unprovisioned = full in self.base_commit_fail_once
            if unprovisioned:
                self.base_commit_fail_once.discard(full)
            commit = None if unprovisioned else self.commits.get(sha)
        if commit is None:
            handler._reply_json(404, {"message": "not found"})
            return
        handler._reply_json(200, {"tree": {"sha": commit["tree"]}})

    def handle_get_compare(self, handler: _Handler, owner: str, repo: str, basehead: str) -> None:
        """`<base>...<head-owner>:<head-branch>` -> GitHub's `status` verdict.

        Only the four `status` values matter to the client: `identical` and
        `behind` mean "not ahead of the base" (nothing unmerged to recover),
        `ahead` and `diverged` mean the branch carries commits the base does
        not. The head is looked up in the same-named repo under `head-owner`
        (the fork convention), matching GitHub's cross-fork compare syntax."""
        base_ref, _, head_ref = basehead.partition("...")
        head_owner, _, head_branch = head_ref.partition(":")
        with self.lock:
            if self.compare_404_once:
                self.compare_404_once = False
                handler._reply_json(404, {"message": "not found"})
                return
            if self.compare_status_once is not None:
                scripted = self.compare_status_once
                self.compare_status_once = None
                handler._reply_json(200, {"status": scripted})
                return
            base_sha = self.refs.get(f"{owner}/{repo}", {}).get(base_ref)
            head_sha = self.refs.get(f"{head_owner}/{repo}", {}).get(head_branch)
            if base_sha is None or head_sha is None:
                handler._reply_json(404, {"message": "not found"})
                return
            if head_sha == base_sha:
                status = "identical"
            elif self._is_ancestor_locked(head_sha, base_sha):
                status = "behind"
            elif self._is_ancestor_locked(base_sha, head_sha):
                status = "ahead"
            else:
                status = "diverged"
        handler._reply_json(200, {"status": status})

    def _is_ancestor_locked(self, ancestor_sha: str, descendant_sha: str) -> bool:
        """Whether `ancestor_sha` is reachable from `descendant_sha` (caller
        holds `self.lock`). The graph is a single parent chain here."""
        cursor: str | None = descendant_sha
        while cursor is not None:
            if cursor == ancestor_sha:
                return True
            cursor = self.commits.get(cursor, {}).get("parent")
        return False

    def _merge_base_locked(self, sha_a: str, sha_b: str) -> str | None:
        """The first commit reachable from both sides, or None when the two
        histories share no ancestor (caller holds `self.lock`).

        The graph is a single parent chain, so "first common ancestor" is
        unambiguous: collect one side's whole chain, then walk the other until
        it lands in it."""
        seen: set[str] = set()
        cursor: str | None = sha_a
        while cursor is not None:
            seen.add(cursor)
            cursor = self.commits.get(cursor, {}).get("parent")
        cursor = sha_b
        while cursor is not None:
            if cursor in seen:
                return cursor
            cursor = self.commits.get(cursor, {}).get("parent")
        return None

    def _tree_locked(self, commit_sha: str | None) -> dict[str, str]:
        """A commit's flattened `{path: blob sha}` tree; empty for no commit."""
        if commit_sha is None:
            return {}
        return self.trees.get(self.commits.get(commit_sha, {}).get("tree", ""), {})

    def _conflicting_locked(
        self, base_full: str, base_branch: str, head_full: str, head_branch: str
    ) -> bool:
        """Whether merging `head` into `base` would conflict (caller holds
        `self.lock`).

        Computed from the object graph, never scripted. A knob would let a test
        pass on an answer a real forge would not give, and both forge surfaces
        answer from here so neither client can agree with a fixture written for
        it alone.

        A file-level three-way against the merge base — which is what git does
        for a whole-file rewrite, and a package root is exactly that. Two
        conditions, both required:

        * the branches must have DIVERGED. One side being an ancestor of the
          other fast-forwards and can never conflict, whatever the trees say.
        * some path must have moved on BOTH sides since the merge base, to
          *different* blobs. A path only one side touched merges cleanly, and so
          does one both sides moved to the same content.

        The second condition is what keeps the detector honest: an unrelated
        package's root advancing the index base is a divergence with no shared
        path, and reading that as a conflict would fire on the ordinary #228
        shape."""
        base_sha = self.refs.get(base_full, {}).get(base_branch)
        head_sha = self.refs.get(head_full, {}).get(head_branch)
        if base_sha is None or head_sha is None:
            return False
        if self._is_ancestor_locked(base_sha, head_sha) or self._is_ancestor_locked(head_sha, base_sha):
            return False
        merge_base_tree = self._tree_locked(self._merge_base_locked(base_sha, head_sha))
        base_tree = self._tree_locked(base_sha)
        head_tree = self._tree_locked(head_sha)
        return any(
            base_tree.get(path) != head_tree.get(path)
            and base_tree.get(path) != merge_base_tree.get(path)
            and head_tree.get(path) != merge_base_tree.get(path)
            for path in base_tree.keys() | head_tree.keys()
        )

    def handle_get_pulls(self, handler: _Handler, owner: str, repo: str, head: str | None, state: str | None) -> None:
        with self.lock:
            record = self.open_prs.get((owner, repo), {}).get(head or "")
        handler._reply_json(200, [record] if record else [])

    def handle_get_pull(self, handler: _Handler, owner: str, repo: str, number: int) -> None:
        """`GET /repos/:owner/:repo/pulls/:number` -> the pull request, carrying
        the `mergeable` tri-state the list endpoint omits.

        `mergeable` is COMPUTED (`_conflicting_locked`), so the answer is one a
        real forge could have given for this graph. It is a plain bool here
        rather than GitHub's `true | false | null`, because the fake has already
        finished computing: `null` means "GitHub is still working on it", a
        state this graph is never in. The client's `null` -> `Unknown` arm is
        pinned by a Rust unit test instead."""
        with self.lock:
            found = next(
                (
                    (head, record)
                    for head, record in self.open_prs.get((owner, repo), {}).items()
                    if record["number"] == number
                ),
                None,
            )
            if found is None:
                handler._reply_json(404, {"message": "not found"})
                return
            head, record = found
            head_owner, _, head_branch = head.partition(":")
            conflicting = self._conflicting_locked(
                f"{owner}/{repo}", record["base"]["ref"], f"{head_owner}/{repo}", head_branch
            )
            body = dict(record) | {"mergeable": not conflicting}
        handler._reply_json(200, body)

    def handle_post_fork(self, handler: _Handler, owner: str, repo: str, body: dict[str, Any]) -> None:
        upstream_full = f"{owner}/{repo}"
        target_owner = body.get("organization")
        key = (upstream_full, target_owner)
        with self.lock:
            record = self.forks.get(key)
            if record is None:
                fork_owner = target_owner or self.token_identity_login
                full_name = self.rename_fork_to or f"{fork_owner}/{repo}"
                fork_owner_actual, fork_repo_actual = full_name.split("/", 1)
                parent_full = self.fork_parent_override or upstream_full
                record = {"full_name": full_name, "owner": fork_owner_actual, "repo": fork_repo_actual}
                self.forks[key] = record
                self.repos[full_name] = {"full_name": full_name, "owner": fork_owner_actual, "parent": parent_full}
                # A fresh fork shares the upstream's git history.
                self.refs[full_name] = dict(self.refs.get(upstream_full, {}))
        handler._reply_json(202, self._repo_body(self.repos[record["full_name"]]))

    def handle_post_blob(self, handler: _Handler, owner: str, repo: str, body: dict[str, Any]) -> None:
        content = base64.b64decode(body.get("content", ""))
        with self.lock:
            sha = self._store_blob_locked(content)
        handler._reply_json(201, {"sha": sha})

    def handle_post_tree(self, handler: _Handler, owner: str, repo: str, body: dict[str, Any]) -> None:
        full = f"{owner}/{repo}"
        with self.lock:
            if full in self.tree_fail_once:
                self.tree_fail_once.discard(full)
                handler._reply_json(404, {"message": "not found (fresh-fork write race)"})
                return
            base_tree_sha = body.get("base_tree")
            merged = dict(self.trees.get(base_tree_sha, {})) if base_tree_sha else {}
            for entry in body.get("tree", []):
                merged[entry["path"]] = entry["sha"]
            tree_sha = self._store_tree_locked(merged)
        handler._reply_json(201, {"sha": tree_sha})

    def handle_post_commit(self, handler: _Handler, owner: str, repo: str, body: dict[str, Any]) -> None:
        tree_sha = body.get("tree")
        parents = body.get("parents") or []
        parent_sha = parents[0] if parents else None
        with self.lock:
            sha = self._store_commit_locked(tree_sha, parent_sha)
        handler._reply_json(201, {"sha": sha})

    def handle_post_ref(self, handler: _Handler, owner: str, repo: str, body: dict[str, Any]) -> None:
        full = f"{owner}/{repo}"
        ref = body.get("ref", "")
        branch = ref.removeprefix("refs/heads/")
        sha = body.get("sha")
        with self.lock:
            self.refs.setdefault(full, {})[branch] = sha
        handler._reply_json(201, {"ref": ref, "object": {"sha": sha}})

    def handle_patch_ref(self, handler: _Handler, owner: str, repo: str, branch: str, body: dict[str, Any]) -> None:
        force = body.get("force")
        if force not in (True, False):
            # The client must STATE the field, never leave it to GitHub's
            # default: a dropped field would silently pick a rewrite policy
            # nobody chose. A missing value answers with a status the client
            # models nowhere, so it surfaces as a hard failure instead of
            # passing through the 422 retry branch.
            handler._reply_json(400, {"message": f"expected an explicit force flag, got {force!r}"})
            return
        full = f"{owner}/{repo}"
        key = f"{full}/{branch}"
        if force:
            # `force: true` repoints the ref unconditionally — no CAS, no
            # ancestry check, and no "reference does not exist" split, because
            # GitHub creates nothing here either way. This is the announce
            # branch-reset path (#228); a concurrent-advance injection is
            # deliberately NOT consulted, since a reset is not racing anyone for
            # a fast-forward.
            with self.lock:
                if branch not in self.refs.get(full, {}):
                    handler._reply_json(422, {"message": "Reference does not exist"})
                    return
                sha = body.get("sha")
                self.refs[full][branch] = sha
            handler._reply_json(200, {"object": {"sha": sha}})
            return
        with self.lock:
            advance = self.concurrent_ref_advance.pop(key, None)
            if advance is not None:
                # A racing announce advanced the branch between our read and our
                # commit: inject its commit (advancing the head), then reject our
                # fast-forward-only update as non-fast-forward (design register
                # C4 amendment F2). The retry re-reads this advanced head.
                if key in self.concurrent_ref_advance_persists:
                    # DX-87: the writer keeps landing, so the regenerated retry
                    # is refused too and the run reaches S-003's exit 75.
                    self.concurrent_ref_advance[key] = advance
                self._seed_files_locked(owner, repo, advance, branch)
                handler._reply_json(422, {"message": "Update is not a fast forward"})
                return
            if branch not in self.refs.get(full, {}):
                # Real GitHub answers **422**, not 404, when this endpoint is
                # PATCHed for a ref that does not exist — the same status it
                # uses for a rejected fast-forward. Verified live against
                # api.github.com. Modelling it as 404 here let the client's
                # then-wrong 404-means-absent split pass the acceptance suite
                # while every first announce failed in production.
                handler._reply_json(422, {"message": "Reference does not exist"})
                return
            sha = body.get("sha")
            self.refs[full][branch] = sha
        handler._reply_json(200, {"object": {"sha": sha}})

    def handle_post_pull(self, handler: _Handler, owner: str, repo: str, body: dict[str, Any]) -> None:
        key = (owner, repo)
        head = body.get("head", "")
        with self.lock:
            if self.pull_fail_once:
                self.pull_fail_once = False
                handler._reply_json(500, {"message": "simulated pull-request open failure"})
                return
            open_prs = self.open_prs.setdefault(key, {})
            existing = open_prs.get(head)
            if existing is not None:
                handler._reply_json(422, {"message": "A pull request already exists"})
                return
            self._pr_counter += 1
            number = self._pr_counter
            # `base` is what the mergeability route compares the head against;
            # GitHub returns it on both the list and the single-request read.
            record = {
                "number": number,
                "html_url": f"{self.base_url}/{owner}/{repo}/pull/{number}",
                "base": {"ref": body.get("base", "main")},
            }
            open_prs[head] = record
        handler._reply_json(201, record)

    def _repo_body(self, record: dict[str, Any]) -> dict[str, Any]:
        full_name = record["full_name"]
        owner = record["owner"]
        parent = record.get("parent")
        # GitHub returns `permissions` only on an authenticated read; the
        # fork-free path's push probe reads `permissions.push` from exactly here.
        body: dict[str, Any] = {
            "full_name": full_name,
            "owner": {"login": owner},
            "permissions": {"push": full_name not in self.no_push_access},
        }
        if parent is not None:
            body["parent"] = {"full_name": parent}
        return body
