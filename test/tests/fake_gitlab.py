# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""GitLab REST v4 surface for the fake forge.

A mixin over `FakeForge` (`fake_forge.py`) that serves the endpoints
`GitLabForge` (`crates/ocx_lib/src/forge/gitlab.rs`) calls, backed by the **same
in-memory git object graph** the GitHub surface uses.

Sharing the graph is the point, not an implementation shortcut. Every announce
scenario can then be run twice — once through `/repos/...`, once through
`/projects/...` — against one oracle, so "the two forges behave the same" is
asserted rather than asserted-about. Two independent fakes would let the two
clients drift into agreeing with their own fixtures and nothing else.

What is modelled faithfully, because the client depends on it:

* a project is addressed by numeric id **or** by percent-encoded path, and a
  nested group path is one segment;
* `last_commit_id` per file action, which is GitLab's only compare-and-swap;
* `start_project` / `start_sha` / `force`, so a commit can be based on a project
  other than the one it lands in;
* comparison as a directed commit list, from which the client derives
  ahead/behind/diverged itself;
* forks being asynchronous, reported through `import_status`.
"""
from __future__ import annotations

import base64
import re
import urllib.parse
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:  # pragma: no cover - typing only
    from fake_forge import _Handler

# `:id` is a numeric id or a percent-encoded path; both arrive already decoded
# by the dispatcher, so the pattern only has to not span a `/`.
_ID = r"[^/]+"

PROJECT_RE = re.compile(rf"^/projects/(?P<id>{_ID})$")
# `GET /projects/:id/repository/branches` — the LIST endpoint, and the ONLY
# branch read a CI job token may make. The single-branch route below is not on
# GitLab's job-token endpoint list and answers 404 there, which is the defect
# #429 reports: the client read that 404 as "no such branch" and the caller as
# "no base ref found to commit onto".
BRANCHES_RE = re.compile(rf"^/projects/(?P<id>{_ID})/repository/branches$")
BRANCH_RE = re.compile(rf"^/projects/(?P<id>{_ID})/repository/branches/(?P<branch>.+)$")
FILE_RAW_RE = re.compile(rf"^/projects/(?P<id>{_ID})/repository/files/(?P<path>.+)/raw$")
FILE_RE = re.compile(rf"^/projects/(?P<id>{_ID})/repository/files/(?P<path>.+)$")
COMPARE_RE = re.compile(rf"^/projects/(?P<id>{_ID})/repository/compare$")
COMMITS_RE = re.compile(rf"^/projects/(?P<id>{_ID})/repository/commits$")
MERGE_REQUESTS_RE = re.compile(rf"^/projects/(?P<id>{_ID})/merge_requests$")
# The SINGLE merge request. `has_conflicts` and `detailed_merge_status` are only
# worth carrying here — the list endpoint answers "which request", this one
# answers "can it merge".
MERGE_REQUEST_RE = re.compile(rf"^/projects/(?P<id>{_ID})/merge_requests/(?P<iid>\d+)$")
FORK_RE = re.compile(rf"^/projects/(?P<id>{_ID})/fork$")
FORKS_RE = re.compile(rf"^/projects/(?P<id>{_ID})/forks$")
# `GET /projects/:id/job_token_scope/allowlist` — read ONLY when the push
# credential is a job token and the publishing project differs from the index
# project (C-029). A fake that answers it unconditionally would let
# `allowlist_read_only_when_cross_project` pass while the client read it every
# time.
JOB_TOKEN_ALLOWLIST_RE = re.compile(rf"^/projects/(?P<id>{_ID})/job_token_scope/allowlist$")
# `GET /projects/:id/job_token_scope/groups_allowlist` — GitLab's SECOND
# admission list, read only after the projects list came back readable and
# without a hit (#430). A group entry admits every project under it, so an index
# that admits its publishers by group admits nobody as far as the projects list
# is concerned.
JOB_TOKEN_GROUPS_ALLOWLIST_RE = re.compile(
    rf"^/projects/(?P<id>{_ID})/job_token_scope/groups_allowlist$"
)
# `GET /users?username=<login>` — GitLab's user lookup (C-028). The bare
# `/users` path with a query, never `/users/<login>`, which is GitHub's
# (`fake_forge.py`).
USERS_RE = re.compile(r"^/users$")

#: GitLab's Developer access level — the lowest that may push a branch.
ACCESS_LEVEL_DEVELOPER = 30
#: What a project reports when it can push, and when it cannot.
ACCESS_LEVEL_NONE = 10
#: Where synthetic group ids start. Groups are their own id space on the real
#: API, so they must not be drawn from the project counter.
GROUP_ID_BASE = 9000


class GitLabRoutes:
    """GitLab REST v4 handlers, mixed into `FakeForge`.

    Besides `FakeForge`'s own state, `gl_get_merge_requests` requires
    `git_http_fixture.GitHttpRoutes` on the host — it calls
    `git_promote_ready_merge_requests_locked` under the lock to materialise the
    merge requests an asynchronous push worker would have created (D-T4). A
    `FakeForge` assembled without that mixin raises `AttributeError` on every
    merge-request poll.
    """

    # ── dispatch ─────────────────────────────────────────────────────────

    def gitlab_get(self, handler: _Handler, path: str, query: dict[str, list[str]]) -> bool:
        """Serve a GET, returning False when the path is not a GitLab route."""
        match = PROJECT_RE.fullmatch(path)
        if match:
            self.gl_get_project(handler, match.group("id"))
            return True

        match = BRANCHES_RE.fullmatch(path)
        if match:
            self.gl_get_branches(handler, match.group("id"), query)
            return True

        match = BRANCH_RE.fullmatch(path)
        if match:
            self.gl_get_branch(handler, match.group("id"), match.group("branch"))
            return True

        match = FILE_RAW_RE.fullmatch(path)
        if match:
            self.gl_get_file(handler, match.group("id"), match.group("path"), query, raw=True)
            return True

        match = FILE_RE.fullmatch(path)
        if match:
            self.gl_get_file(handler, match.group("id"), match.group("path"), query, raw=False)
            return True

        match = COMPARE_RE.fullmatch(path)
        if match:
            self.gl_get_compare(handler, match.group("id"), query)
            return True

        match = FORKS_RE.fullmatch(path)
        if match:
            self.gl_get_forks(handler, match.group("id"), query)
            return True

        match = MERGE_REQUESTS_RE.fullmatch(path)
        if match:
            self.gl_get_merge_requests(handler, match.group("id"), query)
            return True

        match = MERGE_REQUEST_RE.fullmatch(path)
        if match:
            self.gl_get_merge_request(handler, match.group("id"), int(match.group("iid")))
            return True

        match = JOB_TOKEN_ALLOWLIST_RE.fullmatch(path)
        if match:
            self.gl_get_job_token_allowlist(handler, match.group("id"), query)
            return True

        match = JOB_TOKEN_GROUPS_ALLOWLIST_RE.fullmatch(path)
        if match:
            self.gl_get_job_token_groups_allowlist(handler, match.group("id"), query)
            return True

        if USERS_RE.fullmatch(path):
            self.gl_get_users(handler, query)
            return True

        return False

    def gitlab_post(self, handler: _Handler, path: str, body: dict[str, Any]) -> bool:
        """Serve a POST, returning False when the path is not a GitLab route."""
        match = COMMITS_RE.fullmatch(path)
        if match:
            self.gl_post_commit(handler, match.group("id"), body)
            return True

        match = FORK_RE.fullmatch(path)
        if match:
            self.gl_post_fork(handler, match.group("id"), body)
            return True

        match = MERGE_REQUESTS_RE.fullmatch(path)
        if match:
            self.gl_post_merge_request(handler, match.group("id"), body)
            return True

        return False

    # ── job-token endpoint scope ─────────────────────────────────────────

    def gl_job_token_refuses(self, handler: _Handler) -> bool:
        """Whether this request is a job token reaching past what it may call.

        Keyed on the header the client actually sent rather than on a fixture
        flag naming the posture: `GitLabForge::request` selects `JOB-TOKEN` when
        the API credential IS the job token, so this asks the same question the
        real instance asks, of the same evidence.

        **Unconditional, with no knob to turn it off.** The permissive answer is
        what let #429 ship — this fake served `GET /projects/:id` and the
        single-branch endpoint to every credential, so the job-token posture
        passed here and died against a real instance at the first read — and a
        scope a row can switch off is one a row can be made to pass against.
        What holds this predicate itself is the pair of `unknown` capability
        statuses `test_transport_git.py`'s
        `::test_job_token_run_reads_only_endpoints_a_job_token_may_call`
        asserts: one per refused route, each red the moment a route starts
        answering.

        **The status is the caller's, and the two callers differ**, because the
        two refusals have different causes. `GET /projects/:id` and the single
        branch answer **404** — what a self-hosted 19.3 was observed to answer,
        and the status the client cannot tell apart from "no such project",
        which is the whole reason #429's failure named the wrong thing. Both
        `job_token_scope` lists answer **401**, which is what the same 19.3 was
        observed to answer a `JOB-TOKEN` header there (#432) — the same status
        the users API gives a job token, and NOT the 403 the Maintainer-or-Owner
        bar suggests. That reasoning is why this file served 403 and why the
        suite was green while the real instance exited 80: a status nobody
        measured, chosen because it read plausibly.

        https://docs.gitlab.com/ci/jobs/ci_job_token/#job-token-access
        """
        return handler.headers.get("JOB-TOKEN") is not None

    # ── project identity ─────────────────────────────────────────────────

    def gl_project_id(self, full_path: str) -> int:
        """The numeric id for a project path, assigning one on first sight."""
        with self.lock:
            return self._gl_project_id_locked(full_path)

    def _gl_project_id_locked(self, full_path: str) -> int:
        existing = self.gitlab_ids.get(full_path)
        if existing is not None:
            return existing
        self._gitlab_id_counter += 1
        self.gitlab_ids[full_path] = self._gitlab_id_counter
        return self._gitlab_id_counter

    def _gl_resolve_locked(self, identifier: str) -> str | None:
        """A `:id` segment -> the project path it names, or None.

        The segment arrives percent-encoded (`acme%2Fplatform%2Findex`) because
        that is how a nested path survives as one segment; it is decoded only
        here, after the route regex has already proved it did not split.
        """
        identifier = urllib.parse.unquote(identifier)
        if identifier.isdigit():
            wanted = int(identifier)
            for path, assigned in self.gitlab_ids.items():
                if assigned == wanted:
                    return path
            return None
        return identifier if identifier in self.repos else None

    def _gl_project_body_locked(self, full_path: str) -> dict[str, Any]:
        record = self.repos[full_path]
        parent = record.get("parent")
        access = ACCESS_LEVEL_NONE if full_path in self.no_push_access else ACCESS_LEVEL_DEVELOPER
        body: dict[str, Any] = {
            "id": self._gl_project_id_locked(full_path),
            "path_with_namespace": full_path,
            "default_branch": "main",
            "import_status": self.gitlab_import_status.get(full_path, "none"),
            # `permissions` is only present on an authenticated read, exactly as
            # on the real API; the push probe reads `project_access.access_level`
            # from here.
            "permissions": {"project_access": {"access_level": access}, "group_access": None},
        }
        if parent is not None:
            body["forked_from_project"] = {"id": self._gl_project_id_locked(parent)}
        # THREE states, and the third one is the absence of the key (C-029). A
        # project not named in the knob emits no field at all — the GitLab <
        # 18.4 / hidden-setting instance, which is the only case yielding
        # `unknown`-and-proceed and therefore the only one C-044's promotion
        # covers. Defaulting to `False` here would refuse every existing
        # consumer at 86 before it pushed; defaulting to `True` would make the
        # unreadable case unreachable.
        #
        #
        # An empty knob is the state of every pre-existing test in this tree, and
        # it stays byte-identical to the body they already see: the key is
        # emitted only for a project the knob names.
        allowed = self.gitlab_job_token_push_allowed.get(full_path)
        if allowed is not None:
            body["ci_push_repository_for_job_token_allowed"] = allowed
        return body

    # ── read routes ──────────────────────────────────────────────────────

    def gl_get_project(self, handler: _Handler, identifier: str) -> None:
        # The Projects API is not on the job-token endpoint list at all.
        if self.gl_job_token_refuses(handler):
            handler._reply_json(404, {"message": "404 Project Not Found"})
            return
        with self.lock:
            full_path = self._gl_resolve_locked(identifier)
            if full_path is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            # A project whose import is still running is visible but not ready;
            # its readiness is reported in the body, never as a 404.
            body = self._gl_project_body_locked(full_path)
            remaining = self.gitlab_import_pending.get(full_path, 0)
            if remaining != 0:
                body["import_status"] = "started"
                if remaining > 0:
                    self.gitlab_import_pending[full_path] = remaining - 1
        handler._reply_json(200, body)

    def gl_get_branch(self, handler: _Handler, identifier: str, branch: str) -> None:
        """`GET /projects/:id/repository/branches/:branch` — the SINGLE branch.

        The Branches API opens only its list endpoint to a job token; this one
        answers 404 there. Keeping the route means a client that regresses to it
        fails here the way it fails in production, instead of being served.
        """
        if self.gl_job_token_refuses(handler):
            handler._reply_json(404, {"message": "404 Branch Not Found"})
            return
        branch = urllib.parse.unquote(branch)
        with self.lock:
            full_path = self._gl_resolve_locked(identifier)
            sha = None if full_path is None else self.refs.get(full_path, {}).get(branch)
        if sha is None:
            handler._reply_json(404, {"message": "404 Branch Not Found"})
            return
        handler._reply_json(200, {"name": branch, "commit": {"id": sha}})

    def gl_get_branches(self, handler: _Handler, identifier: str, query: dict[str, list[str]]) -> None:
        """`GET /projects/:id/repository/branches?search=&per_page=` -> a LIST.

        `search` is GitLab's own filter, and `^term` is its PREFIX form — not an
        anchor and not a regex, so `^main` returns `maintenance` and `main-old`
        alongside `main`. Modelled faithfully because the exact-name selection is
        the client's, and a fake that answered only the exact match would make
        that selection unfalsifiable — a `[0]` implementation would pass.

        A project that is not visible is 404; a search that matches nothing is
        **200 with an empty list**. Those are different answers, and the client
        folds only the second one to "the branch does not exist".
        """
        search = (query.get("search") or [""])[0]
        try:
            per_page = int((query.get("per_page") or ["20"])[0])
        except ValueError:
            per_page = 20
        with self.lock:
            full_path = self._gl_resolve_locked(identifier)
            if full_path is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            refs = dict(self.refs.get(full_path, {}))
        if not search:
            matched = list(refs)
        elif search.startswith("^"):
            matched = [name for name in refs if name.startswith(search[1:])]
        else:
            matched = [name for name in refs if search in name]
        # #436's premise, armed per read and charged only against a name this
        # listing would otherwise have returned. A denied name is withheld from
        # the LISTING only — the bare repository keeps its ref and a raw read at a
        # sha still answers — which is the disagreement the report describes and
        # the only one that produces the defect.
        #
        # After the search filter, not before: the announce path lists `^main`
        # too, and charging every armed name on every listing would spend the
        # count on a read that never asked about that branch. Here rather than in
        # `gl_get_branch` because the client resolves a branch through the list
        # (#429), and that single-branch route already 404s a job token.
        with self.lock:
            for name in list(matched):
                key = f"{full_path}/{name}"
                remaining = self.gitlab_branch_reads_denied.get(key, 0)
                if remaining > 0:
                    self.gitlab_branch_reads_denied[key] = remaining - 1
                    matched.remove(name)
                    del refs[name]
        # Reverse order, deliberately. GitLab orders by name, under which the
        # exact match sorts FIRST among its own prefixes (`main` before
        # `main-old` before `maintenance`) — and a fixture that hands the wanted
        # entry back first lets a client that reads `[0]` pass every row here
        # while picking a neighbouring branch against a real instance. The
        # ordering is not part of the contract; the client's exact-name selection
        # is, so this answers in the order that can falsify it.
        names = sorted(matched, reverse=True)
        entries = [{"name": name, "commit": {"id": refs[name]}} for name in names[:per_page]]
        handler._reply_json(200, entries)

    def gl_get_file(self, handler: _Handler, identifier: str, path: str, query: dict[str, list[str]], *, raw: bool) -> None:
        path = urllib.parse.unquote(path)
        ref = (query.get("ref") or ["main"])[0]
        with self.lock:
            full_path = self._gl_resolve_locked(identifier)
            if full_path is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            commit_sha = self.refs.get(full_path, {}).get(ref)
            if commit_sha is None and ref in self.commits:
                commit_sha = ref
            if commit_sha is None:
                handler._reply_json(404, {"message": "404 Commit Not Found"})
                return
            tree = self.trees[self.commits[commit_sha]["tree"]]
            blob_sha = tree.get(path)
            if blob_sha is None:
                handler._reply_json(404, {"message": "404 File Not Found"})
                return
            content = self.blobs[blob_sha]
            last_commit_id = self.file_last_commit.get(commit_sha, {}).get(path, commit_sha)
        if raw:
            handler._reply_raw(200, content)
            return
        handler._reply_json(
            200,
            {
                "file_path": path,
                "ref": ref,
                "blob_id": blob_sha,
                "last_commit_id": last_commit_id,
                "encoding": "base64",
                "content": base64.b64encode(content).decode(),
            },
        )

    def gl_get_compare(self, handler: _Handler, identifier: str, query: dict[str, list[str]]) -> None:
        """`from` (in `from_project_id`) -> `to` (in `:id`), as a commit list.

        GitLab publishes no ahead/behind verdict; the client derives one by
        asking twice. Returning the commits `to` carries that `from` does not is
        the whole contract.
        """
        from_ref = (query.get("from") or [""])[0]
        to_ref = (query.get("to") or [""])[0]
        from_project = (query.get("from_project_id") or [identifier])[0]
        with self.lock:
            if self.gitlab_compare_malformed_once:
                self.gitlab_compare_malformed_once = False
                handler._reply_json(200, {"compare_timeout": False})
                return
            if self.gitlab_compare_timeout_once:
                self.gitlab_compare_timeout_once = False
                handler._reply_json(200, {"commits": [], "compare_timeout": True})
                return
            to_path = self._gl_resolve_locked(identifier)
            from_path = self._gl_resolve_locked(from_project)
            if to_path is None or from_path is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            to_sha = self.refs.get(to_path, {}).get(to_ref) or (to_ref if to_ref in self.commits else None)
            from_sha = self.refs.get(from_path, {}).get(from_ref) or (from_ref if from_ref in self.commits else None)
            if to_sha is None or from_sha is None:
                handler._reply_json(404, {"message": "404 Ref Not Found"})
                return
            reachable_from = set()
            cursor: str | None = from_sha
            while cursor is not None:
                reachable_from.add(cursor)
                cursor = self.commits.get(cursor, {}).get("parent")
            commits = []
            cursor = to_sha
            while cursor is not None and cursor not in reachable_from:
                commits.append({"id": cursor})
                cursor = self.commits.get(cursor, {}).get("parent")
        handler._reply_json(200, {"commits": commits, "compare_timeout": False})

    def gl_get_forks(self, handler: _Handler, identifier: str, query: dict[str, list[str]]) -> None:
        page = int((query.get("page") or ["1"])[0])
        with self.lock:
            upstream = self._gl_resolve_locked(identifier)
            if upstream is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            entries = [
                self._gl_project_body_locked(full_path)
                for full_path, record in sorted(self.repos.items())
                if record.get("parent") == upstream
            ]
        # One page is enough for every scenario the suite runs; a second page is
        # always empty, which is also what ends the client's walk.
        handler._reply_json(200, entries if page == 1 else [])

    def gl_get_job_token_allowlist(
        self, handler: _Handler, identifier: str, query: dict[str, list[str]]
    ) -> None:
        """`GET /projects/:id/job_token_scope/allowlist` -> the projects allowed
        to push here with their own job token.

        Three outcomes (C-029): a list CONTAINING the publishing project passes;
        a list without it is a miss and refuses at 86 naming both project paths;
        a refusal is `unreadable`, which proceeds. An absent key is an EMPTY list —
        a miss — and never an unreadable one: a single "absent means unreadable"
        rule would make the miss unreachable, and the two produce different exit
        codes.

        **Offset-paginated, like the real endpoint**, and that is not decoration.
        GitLab's default page is 20 while the client asks for 100 and walks to a
        short page; a fake that answered every page in full would let a
        single-page client pass, and a single-page client turns a publishing
        project on page two into a false 86 before any push. Serving the
        requested window is what makes the walk observable at all.

        Refusal is the **production-common** answer, not an edge case: the
        endpoint requires Maintainer or Owner on the index project while the
        preflight's own bar is Developer, so the publisher this check exists for
        reads `unknown` rather than a verdict. That makes the 86-miss path
        reachable only under a credential that clears the bar — the split pair —
        which the consuming rows say in their own doc comments.
        """
        # 401, not the 404 the project and branch routes answer, and not the 403
        # the Maintainer bar suggests: a self-hosted 19.3 answers a `JOB-TOKEN`
        # header here with `401 Unauthorized` (#432). Serving anything else hands
        # the client a refusal shape no real instance sends — which is how both
        # #429 and #432 shipped out of this file.
        if self.gl_job_token_refuses(handler):
            handler._reply_json(401, {"message": "401 Unauthorized"})
            return
        per_page = int((query.get("per_page") or ["20"])[0])
        page = int((query.get("page") or ["1"])[0])
        with self.lock:
            target = self._gl_resolve_locked(identifier)
            if target is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            start = (page - 1) * per_page
            window = self.gitlab_job_token_allowlist.get(target, [])[start : start + per_page]
            entries = [
                {"id": self._gl_project_id_locked(source), "path_with_namespace": source}
                for source in window
            ]
        handler._reply_json(200, entries)

    def gl_get_job_token_groups_allowlist(
        self, handler: _Handler, identifier: str, query: dict[str, list[str]]
    ) -> None:
        """`GET /projects/:id/job_token_scope/groups_allowlist` -> the GROUPS
        allowed to push here with a job token of any project under them (#430).

        Same three outcomes and the same offset pagination as the projects list
        above, deliberately — GitLab documents both endpoints identically, down
        to the 401 a job token is refused with (#432).

        The pagination is walked by
        `test_transport_git.py::test_allowlist_group_admission_is_walked_past_the_first_page`,
        which puts the admitting group on page two; every other group row seeds
        one entry and would pass against a handler that ignored the window.

        **The body is the real one, and that is the load-bearing part.** A groups
        entry carries `id`, `web_url` and `name` and NOTHING else: no
        `full_path`, no `path`, no `path_with_namespace`. Emitting a path field
        the API does not send would let a client that read the wrong key pass
        here and fail against a real instance — which is precisely how #429
        shipped out of this same file.

        https://docs.gitlab.com/api/project_job_token_scopes/
        """
        # 401, for the same reason and from the same observation as the projects
        # list above (#432): GitLab documents both endpoints identically and a
        # 19.3 refuses both the same way.
        if self.gl_job_token_refuses(handler):
            handler._reply_json(401, {"message": "401 Unauthorized"})
            return
        per_page = int((query.get("per_page") or ["20"])[0])
        page = int((query.get("page") or ["1"])[0])
        with self.lock:
            target = self._gl_resolve_locked(identifier)
            if target is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            start = (page - 1) * per_page
            window = self.gitlab_job_token_groups_allowlist.get(target, [])[start : start + per_page]
        entries = [
            {
                # Positional, and deliberately not drawn from the project-id
                # counter: groups are a separate id space on the real API, and
                # the client reads neither — the only identity it holds is
                # `CI_PROJECT_PATH`, which is the whole point of #430.
                "id": GROUP_ID_BASE + start + offset,
                "name": group.rsplit("/", 1)[-1],
                "web_url": f"{self.base_url}/groups/{group}",
            }
            for offset, group in enumerate(window)
        ]
        handler._reply_json(200, entries)

    def gl_get_users(self, handler: _Handler, query: dict[str, list[str]]) -> None:
        """`GET /users?username=<login>` -> GitLab's user lookup (C-028).

        Answers a LIST, as the real API does: an empty list is `Ok(None)`, and
        the entry's `bot` field is the forge's own assertion that sets
        `ForgeIdentity::bot`.

        Two things about the shape are load-bearing and neither is arbitrary. An
        **unknown login is 200 with `[]`**, never a 404: GitLab answers a
        collection here, `OwnerUnknown` (79) is decided above the client, and a
        404 would move that decision into the fixture. And the answer is a
        LIST — a dict-shaped body would deserialise for a client that reads one
        field and diverge from the real API only under a second match.

        The lookup case-folds and answers the CANONICAL spelling, sharing
        `seed_user`'s store with GitHub's `/users/<login>`, so the same account
        confirms identically on both surfaces (C-048).

        `users_api_status` stays deferred — a knob echo, no consumer in this
        package; it reaches the caller as a 501 (`_Handler._reply_stub`).

        **This route is where the `asserted` vocabulary lives (DX-72).**
        `owner_identity_source: "asserted"` requires `confirm_with_forge` to see
        `Err(UsersApiUnavailable)`, and only GitLab produces it — GitHub's
        `resolve_user` maps 404 to `Ok(None)` and every other non-success to
        `Err(Status)`, so `asserted` is **unreachable on GitHub**. GitLab
        produces it only under `users_api_is_out_of_reach`, which additionally
        requires `api_is_job_token`, so arming this knob alone is not enough:
        the run must also carry `OCX_ANNOUNCE_TOKEN` equal to a non-empty
        `CI_JOB_TOKEN`. Consumers: `test_package_claim.py`'s `asserted` row and
        `::test_owner_ci_environment`'s GitLab half. When this is implemented,
        the status must be 401 or 403 — no other value satisfies the Rust
        client's predicate — and the empty-list arm below must stay a **200**,
        because a 404 there would move `OwnerUnknown` into the fixture.
        """
        if self.users_api_status is not None:
            handler._reply_json(
                self.users_api_status,
                {"message": "the users API is not readable with this credential"},
            )
            return
        login = (query.get("username") or [""])[0]
        with self.lock:
            user = self.users.get(login.lower()) if login else None
        if user is None:
            handler._reply_json(200, [])
            return
        handler._reply_json(
            200, [{"username": user.login, "id": user.id, "bot": user.bot}]
        )

    def gl_get_merge_requests(self, handler: _Handler, identifier: str, query: dict[str, list[str]]) -> None:
        source_branch = (query.get("source_branch") or [""])[0]
        source_project = (query.get("source_project_id") or [""])[0]
        with self.lock:
            # The asynchronous worker D-T4 describes, as a LAZY gate: a push's
            # `post-receive` hook wrote `ready_at` and exited immediately, and
            # readiness is decided here, at read time. A sleeping hook would
            # hold the CGI child and the HTTP response open and wedge the very
            # push it is meant to complete. A no-op when no push has been
            # recorded, which is every existing consumer.
            self.git_promote_ready_merge_requests_locked()
            target = self._gl_resolve_locked(identifier)
            if source_project:
                source = self._gl_resolve_locked(source_project)
                found = self.gitlab_merge_requests.get((target, source, source_branch))
                records = [found] if found else []
            else:
                # An ABSENT `source_project_id` is no narrowing, exactly as on the
                # real API: every open request onto this target from a branch of
                # this name, whatever project it came from. Answering `[]` here —
                # which keying the lookup on `source=None` amounts to — would let
                # a client that stopped sending the filter see no request at all,
                # and it stopped sending it on purpose: resolving the numeric id
                # the filter needs costs a `GET /projects/:id`, which a CI job
                # token may not call (ocx#429). The narrowing moved into the
                # client, which compares each entry's own source and target ids,
                # so this route has to hand it the entries to compare.
                records = [
                    record
                    for (record_target, _source, record_branch), record in self.gitlab_merge_requests.items()
                    if record_target == target and record_branch == source_branch
                ]
        handler._reply_json(200, records)

    def gl_get_merge_request(self, handler: _Handler, identifier: str, iid: int) -> None:
        """`GET /projects/:id/merge_requests/:iid` -> the merge request, with the
        two fields the client reads to decide mergeability.

        Both are COMPUTED by `_conflicting_locked` (`fake_forge.py`) — the same
        helper the GitHub surface answers `mergeable` from, over the same object
        graph. That is the point of one graph: the two clients are held to one
        oracle instead of to a per-surface knob that could tell each of them
        what it wants to hear.

        Only the settled pair is emitted. GitLab's in-progress values
        (`checking`, `unchecked`) mean "not computed yet", which this fake never
        is; the client's arm for them is pinned by a Rust unit test."""
        with self.lock:
            target = self._gl_resolve_locked(identifier)
            if target is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            found = next(
                (
                    (key, record)
                    for key, record in self.gitlab_merge_requests.items()
                    if key[0] == target and record["iid"] == iid
                ),
                None,
            )
            if found is None:
                handler._reply_json(404, {"message": "404 Merge Request Not Found"})
                return
            (_, source, source_branch), record = found
            conflicting = self._conflicting_locked(target, record["target_branch"], source, source_branch)
            body = dict(record) | {
                "has_conflicts": conflicting,
                "detailed_merge_status": "conflict" if conflicting else "mergeable",
            }
        handler._reply_json(200, body)

    # ── write routes ─────────────────────────────────────────────────────

    def gl_post_commit(self, handler: _Handler, identifier: str, body: dict[str, Any]) -> None:
        branch = body.get("branch", "")
        start_sha = body.get("start_sha")
        start_project = body.get("start_project")
        force = bool(body.get("force"))
        actions = body.get("actions") or []
        with self.lock:
            target = self._gl_resolve_locked(identifier)
            if target is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return

            head = self.refs.get(target, {}).get(branch)
            if head is not None and start_sha is not None and not force:
                # GitLab refuses to (re)start an existing branch without force.
                handler._reply_json(400, {"message": f"A branch called '{branch}' already exists."})
                return

            if start_sha is not None:
                base_sha = start_sha
                if start_project is not None:
                    source = self._gl_resolve_locked(str(start_project))
                    if source is None:
                        handler._reply_json(404, {"message": "404 Project Not Found"})
                        return
            elif head is not None:
                base_sha = head
            else:
                handler._reply_json(400, {"message": "You can only create or edit files when you are on a branch"})
                return

            if base_sha not in self.commits:
                handler._reply_json(400, {"message": "404 Commit Not Found"})
                return

            racing = self.gitlab_concurrent_advance.pop(f"{target}/{branch}", None)
            if racing is not None:
                # A racing announce lands first, moving the root's last commit.
                self._seed_files_locked(target.split("/")[0], target.split("/", 1)[1], racing, branch)
                head = self.refs[target][branch]

            tree = dict(self.trees[self.commits[base_sha]["tree"]])
            if head is not None and start_sha is None:
                tree = dict(self.trees[self.commits[head]["tree"]])
            for action in actions:
                path = action["file_path"]
                kind = action["action"]
                present = path in tree
                if kind == "create" and present:
                    handler._reply_json(400, {"message": f"A file with the name {path} already exists"})
                    return
                if kind == "update" and not present:
                    handler._reply_json(400, {"message": f"A file with the name {path} doesn't exist"})
                    return
                if kind == "update":
                    # The compare-and-swap. `last_commit_id` names the commit the
                    # editor based its version on; anything newer means somebody
                    # else changed this file first.
                    claimed = action.get("last_commit_id")
                    # The editor based its version on whatever it started from:
                    # the branch head when accumulating, the explicit start
                    # commit when the branch is being created or rebuilt. Judging
                    # a rebuild against the stale branch head instead would
                    # reject every legitimate reset.
                    actual_root = base_sha if start_sha is not None else head
                    actual = self.file_last_commit.get(actual_root, {}).get(path)
                    if claimed is not None and actual is not None and claimed != actual:
                        handler._reply_json(
                            400,
                            {
                                "message": "You are attempting to update a file that has changed "
                                "since you started editing it."
                            },
                        )
                        return
                content = base64.b64decode(action.get("content", ""))
                tree[path] = self._store_blob_locked(content)

            parent = head if (head is not None and start_sha is None) else base_sha
            tree_sha = self._store_tree_locked(tree)
            commit_sha = self._store_commit_locked(tree_sha, parent)
            self.refs.setdefault(target, {})[branch] = commit_sha
        handler._reply_json(201, {"id": commit_sha, "parent_ids": [parent] if parent else []})

    def gl_post_fork(self, handler: _Handler, identifier: str, body: dict[str, Any]) -> None:
        namespace = body.get("namespace_path")
        with self.lock:
            upstream = self._gl_resolve_locked(identifier)
            if upstream is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            project = upstream.split("/")[-1]
            existing = [
                full for full, record in self.repos.items() if record.get("parent") == upstream and full.rsplit("/", 1)[0] == namespace
            ]
            if existing:
                handler._reply_json(409, {"message": "409 Conflict: Project already forked"})
                return
            full_path = self.gitlab_rename_fork_to or f"{namespace}/{project}"
            parent = self.gitlab_fork_parent_override or upstream
            self.repos[full_path] = {
                "full_name": full_path,
                "owner": full_path.split("/")[0],
                "parent": parent,
            }
            self.refs[full_path] = dict(self.refs.get(upstream, {}))
            self.gitlab_import_status[full_path] = "finished"
            body_out = self._gl_project_body_locked(full_path)
        handler._reply_json(201, body_out)

    def gl_post_merge_request(self, handler: _Handler, identifier: str, body: dict[str, Any]) -> None:
        source_branch = body.get("source_branch", "")
        with self.lock:
            source = self._gl_resolve_locked(identifier)
            if source is None:
                handler._reply_json(404, {"message": "404 Project Not Found"})
                return
            target_project = body.get("target_project_id")
            target = self._gl_resolve_locked(str(target_project)) if target_project is not None else source
            if target is None:
                handler._reply_json(404, {"message": "404 Target Project Not Found"})
                return
            if self.gitlab_merge_request_fail_once:
                self.gitlab_merge_request_fail_once = False
                handler._reply_json(500, {"message": "simulated merge-request open failure"})
                return
            key = (target, source, source_branch)
            if key in self.gitlab_merge_requests:
                handler._reply_json(409, {"message": "409 Conflict: Another open merge request already exists"})
                return
            self._gitlab_mr_counter += 1
            number = self._gitlab_mr_counter
            record = {
                "id": 90000 + number,
                "iid": number,
                "web_url": f"{self.base_url}/{target}/-/merge_requests/{number}",
                "state": "opened",
                # The branch the request targets — what the single-request route
                # compares the source branch against.
                "target_branch": body.get("target_branch", "main"),
                # And the branch it comes FROM. Carried so a request this route
                # created is indistinguishable from one
                # `git_promote_ready_merge_requests_locked` materialised out of
                # a push option (D-T4): two shapes would let a poll assert on a
                # key only one of the two writers emits.
                "source_branch": source_branch,
                # The provenance pair. Carried by every real merge-request body,
                # and the only thing that distinguishes this request from one a
                # stranger's fork opened onto the same index on the same branch
                # name — the client compares them rather than resolving a numeric
                # project id, which a CI job token cannot read (ocx#429).
                "source_project_id": self._gl_project_id_locked(source),
                "target_project_id": self._gl_project_id_locked(target),
            }
            self.gitlab_merge_requests[key] = record
        handler._reply_json(201, record)

    # ── test scripting helpers ───────────────────────────────────────────

    def gitlab_close_merge_request(self, target: str, source: str, branch: str) -> None:
        """Drop the open merge request, leaving its branch in place — the GitLab
        half of the trap #228 is about."""
        with self.lock:
            self.gitlab_merge_requests.pop((target, source, branch), None)

    def gitlab_seed_project(self, full_path: str) -> int:
        """Register a project (and its id) without any commit."""
        with self.lock:
            self.repos.setdefault(full_path, {"full_name": full_path, "owner": full_path.split("/")[0], "parent": None})
            return self._gl_project_id_locked(full_path)


def parse_gitlab_path(raw_path: str) -> str:
    """Decode a `:id` or file-path segment inside a GitLab route.

    The client sends `acme%2Fplatform%2Findex` as ONE segment. `urlsplit`
    leaves it encoded, so it is decoded here — after the route regex has already
    seen it as a single segment, which is exactly the property being tested.
    """
    return urllib.parse.unquote(raw_path)


__all__ = ["GitLabRoutes", "parse_gitlab_path"]
