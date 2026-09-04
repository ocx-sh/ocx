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

#: GitLab's Developer access level — the lowest that may push a branch.
ACCESS_LEVEL_DEVELOPER = 30
#: What a project reports when it can push, and when it cannot.
ACCESS_LEVEL_NONE = 10


class GitLabRoutes:
    """GitLab REST v4 handlers, mixed into `FakeForge`."""

    # ── dispatch ─────────────────────────────────────────────────────────

    def gitlab_get(self, handler: _Handler, path: str, query: dict[str, list[str]]) -> bool:
        """Serve a GET, returning False when the path is not a GitLab route."""
        match = PROJECT_RE.fullmatch(path)
        if match:
            self.gl_get_project(handler, match.group("id"))
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
        return body

    # ── read routes ──────────────────────────────────────────────────────

    def gl_get_project(self, handler: _Handler, identifier: str) -> None:
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
        branch = urllib.parse.unquote(branch)
        with self.lock:
            full_path = self._gl_resolve_locked(identifier)
            sha = None if full_path is None else self.refs.get(full_path, {}).get(branch)
        if sha is None:
            handler._reply_json(404, {"message": "404 Branch Not Found"})
            return
        handler._reply_json(200, {"name": branch, "commit": {"id": sha}})

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

    def gl_get_merge_requests(self, handler: _Handler, identifier: str, query: dict[str, list[str]]) -> None:
        source_branch = (query.get("source_branch") or [""])[0]
        source_project = (query.get("source_project_id") or [""])[0]
        with self.lock:
            target = self._gl_resolve_locked(identifier)
            source = self._gl_resolve_locked(source_project) if source_project else None
            key = (target, source, source_branch)
            record = self.gitlab_merge_requests.get(key)
        handler._reply_json(200, [record] if record else [])

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
