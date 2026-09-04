# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Both polarities of the fake forge's computed mergeability.

A detector that only ever says one thing is not a detector, and the announce
tests that consume these routes cannot tell the two apart on their own: a
scenario asserting "this run is refused" passes just as well against a fake that
reports every pull request as conflicting. So the fake's own answer is pinned
here, on two graphs that differ in exactly one way — whether the index base
moved the *same* file the announce branch did.

Driven over HTTP rather than by calling the helper, because the routes are half
of what WP3's clients depend on: a helper that answers correctly behind a route
that never matches is still a fake that reports nothing.
"""
from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any

INDEX_OWNER = "ocx-sh"
INDEX_REPO = "index"
FORK_OWNER = "forkuser"
INDEX_PATH = f"{INDEX_OWNER}/{INDEX_REPO}"
FORK_PATH = f"{FORK_OWNER}/{INDEX_REPO}"
BRANCH = "indexbot-announce-acme-widget"

#: The package this announce branch owns.
ROOT = "p/acme/widget.json"
#: Another package's root, sharing the index but nothing else.
UNRELATED = "p/other/package.json"


def _get(base_url: str, path: str) -> tuple[int, Any]:
    try:
        with urllib.request.urlopen(f"{base_url}{path}") as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())


def _post(base_url: str, path: str, payload: dict[str, Any]) -> Any:
    request = urllib.request.Request(
        f"{base_url}{path}",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request) as response:
        return json.loads(response.read())


def _diverge(fake, moved_on_base: str) -> None:
    """Build the announce shape: a branch and its base that both moved on.

    Common to both polarities except for `moved_on_base` — the one path the
    index base rewrites after the announce branch was cut. That single
    difference is the whole experiment.
    """
    fake.seed_files(
        INDEX_OWNER,
        INDEX_REPO,
        {ROOT: b'{"tags":{"1.0.0":{}}}', UNRELATED: b'{"tags":{"9.0.0":{}}}'},
        branch="main",
    )
    fake.seed_branch_at(
        FORK_OWNER,
        INDEX_REPO,
        BRANCH,
        source_owner=INDEX_OWNER,
        source_repo=INDEX_REPO,
    )
    # The announce branch adds a tag to its own package's root, nothing else.
    fake.seed_files(FORK_OWNER, INDEX_REPO, {ROOT: b'{"tags":{"1.0.0":{},"2.0.0":{}}}'}, branch=BRANCH)
    # The index base then moves on underneath it.
    fake.seed_files(INDEX_OWNER, INDEX_REPO, {moved_on_base: b'{"owners":["acme"],"tags":{}}'}, branch="main")


def _open_pull_request(fake) -> int:
    return _post(
        fake.base_url,
        f"/repos/{INDEX_PATH}/pulls",
        {"title": "announce", "head": f"{FORK_OWNER}:{BRANCH}", "base": "main", "body": ""},
    )["number"]


def _open_merge_request(fake) -> tuple[int, int]:
    index_id = fake.gl_project_id(INDEX_PATH)
    fork_id = fake.gl_project_id(FORK_PATH)
    record = _post(
        fake.base_url,
        f"/projects/{fork_id}/merge_requests",
        {
            "source_branch": BRANCH,
            "target_branch": "main",
            "target_project_id": index_id,
            "title": "announce",
            "description": "",
        },
    )
    return index_id, record["iid"]


def test_an_unrelated_path_moving_the_base_is_not_a_conflict(fake_forge) -> None:
    """The #228 shape. Both sides moved, but not on the same file — git merges
    that cleanly, and a fake that called it a conflict would fire the announce
    tripwire on every ordinary index-wide change."""
    _diverge(fake_forge, moved_on_base=UNRELATED)

    status, pull = _get(fake_forge.base_url, f"/repos/{INDEX_PATH}/pulls/{_open_pull_request(fake_forge)}")
    assert status == 200
    assert pull["mergeable"] is True

    index_id, iid = _open_merge_request(fake_forge)
    status, merge_request = _get(fake_forge.base_url, f"/projects/{index_id}/merge_requests/{iid}")
    assert status == 200
    assert merge_request["has_conflicts"] is False
    assert merge_request["detailed_merge_status"] == "mergeable"


def test_the_same_path_moving_on_both_sides_is_a_conflict(fake_forge) -> None:
    """The #399 shape. The index base rewrote the very root this announce branch
    is rebuilding, so the pull request cannot merge — the state that froze 34
    packages for up to 21 days."""
    _diverge(fake_forge, moved_on_base=ROOT)

    status, pull = _get(fake_forge.base_url, f"/repos/{INDEX_PATH}/pulls/{_open_pull_request(fake_forge)}")
    assert status == 200
    assert pull["mergeable"] is False

    index_id, iid = _open_merge_request(fake_forge)
    status, merge_request = _get(fake_forge.base_url, f"/projects/{index_id}/merge_requests/{iid}")
    assert status == 200
    assert merge_request["has_conflicts"] is True
    assert merge_request["detailed_merge_status"] == "conflict"


def test_a_request_that_does_not_exist_is_a_404_on_both_surfaces(fake_forge) -> None:
    """Both clients read "absent" as no verdict rather than an error, so the
    fake must answer absent as a 404 and not as a 200 carrying nothing."""
    _diverge(fake_forge, moved_on_base=UNRELATED)

    status, _ = _get(fake_forge.base_url, f"/repos/{INDEX_PATH}/pulls/4242")
    assert status == 404

    index_id = fake_forge.gl_project_id(INDEX_PATH)
    status, _ = _get(fake_forge.base_url, f"/projects/{index_id}/merge_requests/4242")
    assert status == 404
