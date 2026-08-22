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
from typing import Any

from fake_gitlab import GitLabRoutes

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
_FORKS_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/forks$")
_BLOBS_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/blobs$")
_TREES_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/trees$")
_COMMITS_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/commits$")
_REFS_CREATE_RE = re.compile(rf"^/repos/(?P<owner>{_SEGMENT})/(?P<repo>{_SEGMENT})/git/refs$")


class _Handler(http.server.BaseHTTPRequestHandler):
    server: FakeForge  # narrows the inherited Any-typed attribute

    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: object) -> None:  # noqa: A002 (stdlib signature)
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

    # ── dispatch ──────────────────────────────────────────────────────────

    def do_GET(self) -> None:  # noqa: N802 (stdlib API)
        parts = urllib.parse.urlsplit(self.path)
        path = parts.path
        query = urllib.parse.parse_qs(parts.query)
        self.server.record("GET", path, self.path)

        if path == "/user":
            # One identity endpoint, two field names: GitHub reads `login`,
            # GitLab reads `username`. Serving both keeps the two surfaces on one
            # account rather than inventing a second test identity.
            self._reply_json(
                200,
                {"login": self.server.token_identity_login, "username": self.server.token_identity_login},
            )
            return

        if self.server.gitlab_get(self, path, query):
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

        self._reply_json(404, {"message": "not found"})

    def do_POST(self) -> None:  # noqa: N802 (stdlib API)
        path = urllib.parse.urlsplit(self.path).path
        self.server.record("POST", path)
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

        self._reply_json(404, {"message": "not found"})

    def do_PATCH(self) -> None:  # noqa: N802 (stdlib API)
        path = urllib.parse.urlsplit(self.path).path
        self.server.record("PATCH", path)
        body = self._read_body()

        match = _REF_UPDATE_RE.fullmatch(path)
        if match:
            self.server.handle_patch_ref(self, match.group("owner"), match.group("repo"), match.group("branch"), body)
            return

        self._reply_json(404, {"message": "not found"})


class FakeForge(GitLabRoutes, http.server.ThreadingHTTPServer):
    """A per-test fake forge (GitHub and GitLab surfaces), bound to an ephemeral
    loopback port."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.requests: list[tuple[str, str]] = []
        # The same log with the query string kept. `requests` is matched
        # exactly by `request_count`, so it cannot carry one; assertions about
        # WHICH ref a read named (a branch name versus a pinned commit) have
        # nowhere else to look.
        self.raw_requests: list[tuple[str, str]] = []
        self.bodies: list[tuple[str, dict[str, Any]]] = []
        self.token_identity_login = "test-forge-bot"

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

        super().__init__(("127.0.0.1", 0), _Handler)

    @property
    def base_url(self) -> str:
        host, port = self.server_address[:2]
        return f"http://{host}:{port}"

    def record(self, method: str, path: str, raw: str | None = None) -> None:
        with self.lock:
            self.requests.append((method, path))
            self.raw_requests.append((method, raw if raw is not None else path))

    def record_body(self, path: str, body: dict[str, Any]) -> None:
        with self.lock:
            self.bodies.append((path, body))

    def request_count(self, method: str, path: str) -> int:
        with self.lock:
            return sum(1 for m, p in self.requests if m == method and p == path)

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
        with self.lock:
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
        too). Distinct from `seed_files`, which always advances the head."""
        source_full = f"{source_owner}/{source_repo}"
        full = f"{owner}/{repo}"
        with self.lock:
            self.refs.setdefault(full, {})[branch] = self.refs[source_full][source_branch]
            self.repos.setdefault(full, {"full_name": full, "owner": owner, "parent": source_full})

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

    def _seed_files_locked(self, owner: str, repo: str, files: dict[str, bytes], branch: str) -> None:
        """Commit `files` onto `branch`, advancing its head (caller holds `self.lock`)."""
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
        self._commit_counter += 1
        key = f"commit:{tree_sha}:{parent_sha}:{self._commit_counter}".encode()
        sha = hashlib.sha1(key).hexdigest()
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

    def handle_get_pulls(self, handler: _Handler, owner: str, repo: str, head: str | None, state: str | None) -> None:
        with self.lock:
            record = self.open_prs.get((owner, repo), {}).get(head or "")
        handler._reply_json(200, [record] if record else [])

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
            record = {"number": number, "html_url": f"{self.base_url}/{owner}/{repo}/pull/{number}"}
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
