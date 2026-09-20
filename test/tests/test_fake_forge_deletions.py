# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The fake forge applies a removal, on both surfaces.

The announce and claim suites drive the fake for everything the real forges do,
so a fake that quietly ignored a deletion would make every future assertion of
the shape "the orphaned object is gone from the index" green without anything
having been removed. The fake's own answer is therefore pinned here rather than
inferred from a scenario that consumes it.

Driven over HTTP and not by calling the helpers, because the routes are half of
what the clients depend on: a handler that drops the right path behind a route
that never matches is still a fake that removes nothing.

The GitLab half also pins the *refusal* — `delete` on a file the base does not
carry is a 400 that fails the whole commit. That is not an incidental fixture
detail: it is the reason `GitLabForge::commit_files_once` reads each path before
it emits an action, and a fake that tolerated the absent delete would let a
driver that skipped the read pass here and fail against a real instance.
"""
from __future__ import annotations

import json
from typing import Any
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen

INDEX_OWNER = "ocx-sh"
INDEX_REPO = "index"
INDEX_PATH = f"{INDEX_OWNER}/{INDEX_REPO}"

#: The package root every commit below keeps.
ROOT = "p/acme/widget.json"
#: A CAS object the root stops referencing — what an orphan sweep removes.
ORPHAN = "p/acme/widget/o/sha256/deadbeef.json"
#: A path no commit ever wrote, which a root may still reference.
NEVER_WRITTEN = "p/acme/widget/o/sha256/cafebabe.json"


def _get_json(base_url: str, path: str) -> tuple[int, Any]:
    try:
        with urlopen(f"{base_url}{path}", timeout=5) as response:
            return response.status, json.loads(response.read())
    except HTTPError as error:
        return error.code, json.loads(error.read())


def _get_status(base_url: str, path: str) -> int:
    """The status alone — the contents route answers raw bytes, not JSON."""
    try:
        with urlopen(f"{base_url}{path}", timeout=5) as response:
            return response.status
    except HTTPError as error:
        return error.code


def _post(base_url: str, path: str, payload: dict[str, Any]) -> tuple[int, Any]:
    request = Request(
        f"{base_url}{path}",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urlopen(request, timeout=5) as response:
            return response.status, json.loads(response.read())
    except HTTPError as error:
        return error.code, json.loads(error.read())


def _seed(fake) -> None:
    fake.seed_files(
        INDEX_OWNER,
        INDEX_REPO,
        {ROOT: b'{"tags":{"1.0.0":{}}}', ORPHAN: b'{"cas":true}'},
        branch="main",
    )


def _github_commit_with_tree(fake, entries: list[dict[str, Any]]) -> tuple[str, str]:
    """Build one commit over the current `main` tree, returning (commit, tree).

    The ref is deliberately left where it was: `handle_get_contents` resolves a
    bare commit sha, so the new tree can be read back without a ref update, and
    the seeded state stays available to the GitLab half of the same fixture.

    The tree sha comes back beside the commit because the routes alone cannot
    tell a *removed* entry from one retained with a null blob sha: both answer
    the contents read with a 404, so a fake that merely stored the null would
    pass every HTTP assertion while carrying a poisoned tree into the merge-base
    comparison and the GitLab file read. The tree itself is the only place that
    distinction is visible.
    """
    status, reference = _get_json(fake.base_url, f"/repos/{INDEX_PATH}/git/ref/heads/main")
    assert status == 200, reference
    head = reference["object"]["sha"]

    status, commit = _get_json(fake.base_url, f"/repos/{INDEX_PATH}/git/commits/{head}")
    assert status == 200, commit

    status, tree = _post(
        fake.base_url,
        f"/repos/{INDEX_PATH}/git/trees",
        {"base_tree": commit["tree"]["sha"], "tree": entries},
    )
    assert status == 201, tree

    status, created = _post(
        fake.base_url,
        f"/repos/{INDEX_PATH}/git/commits",
        {"message": "announce acme/widget", "tree": tree["sha"], "parents": [head]},
    )
    assert status == 201, created
    return created["sha"], tree["sha"]


def _null_entry(path: str) -> dict[str, Any]:
    return {"path": path, "mode": "100644", "type": "blob", "sha": None}


def test_a_null_tree_entry_drops_the_path(fake_forge) -> None:
    """GitHub spells a removal as a tree entry whose `sha` is null. Without the
    arm the path survives every commit and an orphan sweep proves nothing."""
    _seed(fake_forge)
    commit, tree = _github_commit_with_tree(fake_forge, [_null_entry(ORPHAN)])

    assert sorted(fake_forge.trees[tree]) == [ROOT], (
        "the entry must LEAVE the tree, not be retained under a null blob sha"
    )
    assert _get_status(fake_forge.base_url, f"/repos/{INDEX_PATH}/contents/{ORPHAN}?ref={commit}") == 404
    assert _get_status(fake_forge.base_url, f"/repos/{INDEX_PATH}/contents/{ROOT}?ref={commit}") == 200, (
        "only the named path leaves the tree"
    )


def test_a_null_tree_entry_for_an_absent_path_is_a_no_op(fake_forge) -> None:
    """The orphan diff is computed from two roots, so it can name an object no
    commit ever wrote. GitHub's own contract does not say what a null sha
    answers there — the fake must not turn it into a 500, and must leave the
    rest of the tree alone."""
    _seed(fake_forge)
    commit, tree = _github_commit_with_tree(fake_forge, [_null_entry(NEVER_WRITTEN)])

    assert sorted(fake_forge.trees[tree]) == sorted([ROOT, ORPHAN]), (
        "a removal that hit nothing adds nothing either — the absent path must not join the tree"
    )
    assert _get_status(fake_forge.base_url, f"/repos/{INDEX_PATH}/contents/{ROOT}?ref={commit}") == 200
    assert _get_status(fake_forge.base_url, f"/repos/{INDEX_PATH}/contents/{ORPHAN}?ref={commit}") == 200, (
        "a removal that hit nothing removes nothing"
    )


def test_a_gitlab_delete_action_drops_the_path(fake_forge) -> None:
    """GitLab spells the same removal as an action, and answers the read that
    follows with a 404."""
    _seed(fake_forge)
    index_id = fake_forge.gl_project_id(INDEX_PATH)

    status, commit = _post(
        fake_forge.base_url,
        f"/projects/{index_id}/repository/commits",
        {
            "branch": "main",
            "commit_message": "announce acme/widget",
            "actions": [{"file_path": ORPHAN, "action": "delete"}],
        },
    )
    assert status == 201, commit

    quoted_orphan = quote(ORPHAN, safe="")
    status, _ = _get_json(fake_forge.base_url, f"/projects/{index_id}/repository/files/{quoted_orphan}?ref=main")
    assert status == 404

    quoted_root = quote(ROOT, safe="")
    status, _ = _get_json(fake_forge.base_url, f"/projects/{index_id}/repository/files/{quoted_root}?ref=main")
    assert status == 200, "only the named path leaves the tree"


def test_a_gitlab_delete_of_an_absent_path_is_refused(fake_forge) -> None:
    """The control for the driver's per-path read. GitLab refuses the whole
    commit, so a driver that emitted the action blind would take the root and
    every CAS object down with it — and a fake that shrugged here would report
    that driver as correct."""
    _seed(fake_forge)
    index_id = fake_forge.gl_project_id(INDEX_PATH)

    status, body = _post(
        fake_forge.base_url,
        f"/projects/{index_id}/repository/commits",
        {
            "branch": "main",
            "commit_message": "announce acme/widget",
            "actions": [{"file_path": NEVER_WRITTEN, "action": "delete"}],
        },
    )
    assert status == 400, body
    assert NEVER_WRITTEN in body["message"]

    quoted_root = quote(ROOT, safe="")
    status, _ = _get_json(fake_forge.base_url, f"/projects/{index_id}/repository/files/{quoted_root}?ref=main")
    assert status == 200, "the refused commit wrote nothing at all"
