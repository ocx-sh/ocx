# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package announce` against GitLab.

Runs the announce contract through `GitLabForge`
(`crates/ocx_lib/src/forge/gitlab.rs`) on the GitLab surface of the shared fake
forge (`fake_gitlab.py`). The two clients speak to **one** in-memory git object
graph, so a scenario run on both forges compares like with like — that is what
`test_both_forges_commit_the_same_root` exists to prove, and it is the reason
this file does not carry its own fixtures.

What is covered here rather than in `test_announce.py`: the GitLab-shaped halves
of the contract — the per-file compare-and-swap that stands in for a ref-level
one, a commit based on a project other than the one it lands in, nested group
paths surviving as a single URL segment, and the forge-selection grammar for
self-hosted instances.
"""
from __future__ import annotations

import json
import re
from pathlib import Path

from announce_helpers import (
    INDEX_FULL,
    INDEX_OWNER,
    INDEX_REPO,
    TOKEN,
    announce,
    announce_json,
    branch_name,
    configure_trusted_hosts,
    registry_host,
    seed_empty_root,
)
from fake_forge import FakeForge
from src.helpers import make_package
from src.runner import OcxRunner

FORK_NAMESPACE = "forkuser"
FORK_FULL = f"{FORK_NAMESPACE}/{INDEX_REPO}"


def _prepare(ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path, *, tag: str = "1.0.0") -> str:
    """Publish a package and seed the claimed-but-empty index root it announces
    against. Returns the logical package name."""
    make_package(ocx, unique_repo, tag, tmp_path, new=True, cascade=False)
    package = f"acme/{unique_repo}"
    seed_empty_root(fake_forge, package, f"oci://{ocx.registry}/{unique_repo}")
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    return package


def _fork_root(fake_forge: FakeForge, package: str, *, namespace: str = FORK_NAMESPACE) -> dict:
    branch = branch_name(package)
    raw = fake_forge.read_file(namespace, INDEX_REPO, f"p/{package}.json", branch=branch)
    assert raw is not None, f"no committed root on {namespace}/{INDEX_REPO}:{branch}"
    return json.loads(raw)


# ── the fork path end to end ──────────────────────────────────────────────


def test_fork_announce_commits_and_opens_a_merge_request(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A first GitLab announce forks, commits the rebuilt root onto the
    per-package branch, and opens a merge request against the index."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    report = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab"
    )

    assert report["status"] == "updated"
    assert report["fork"] == FORK_FULL
    assert report["pull_request_url"], "a merge request must be reported"
    assert "/-/merge_requests/" in report["pull_request_url"], "the URL must be GitLab's merge-request shape"
    assert report["pull_request_number"] == 1

    root = _fork_root(fake_forge, package)
    assert "1.0.0" in root["tags"], "the announced tag must reach the committed root"


def test_both_forges_commit_the_same_root(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The same announce, run through each client, produces byte-identical roots.

    This is the file's load-bearing assertion. Both clients write into one object
    graph, so the comparison is of what each actually committed, not of each
    against a fixture written for it — the failure mode where two forges each
    agree with their own fake and with nothing else.

    The two runs target different fork namespaces so neither can read the other's
    branch; only the content is shared.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    announce(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", f"gh-fork/{INDEX_REPO}")
    announce(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", f"gl-fork/{INDEX_REPO}", forge="gitlab"
    )

    branch = branch_name(package)
    github_bytes = fake_forge.read_file("gh-fork", INDEX_REPO, f"p/{package}.json", branch=branch)
    gitlab_bytes = fake_forge.read_file("gl-fork", INDEX_REPO, f"p/{package}.json", branch=branch)
    assert github_bytes is not None and gitlab_bytes is not None, "both forges must have committed a root"
    assert github_bytes == gitlab_bytes, "the two forges must commit byte-identical roots"


def test_second_announce_accumulates_onto_the_live_branch(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Two announces before a merge land in one merge request carrying both tags
    (C4): the second bases on the branch head, not on the index base."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False, cascade=False)

    first = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab"
    )
    second = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0,2.0.0", "--fork", FORK_FULL, forge="gitlab"
    )

    assert second["pull_request_number"] == first["pull_request_number"], "the open merge request must be reused"
    root = _fork_root(fake_forge, package)
    assert set(root["tags"]) >= {"1.0.0", "2.0.0"}, "both announces must survive in one branch"


def test_an_unchanged_rerun_commits_nothing(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C6: a run that moves nothing makes no commit and opens no second merge
    request. The branch head is the evidence — it must not advance."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    announce(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab")
    head_before = fake_forge.branch_head(FORK_NAMESPACE, INDEX_REPO, branch_name(package))
    assert head_before is not None, "precondition: the first announce must have committed"

    report = announce_json(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab"
    )

    assert report["status"] == "unchanged"
    assert fake_forge.branch_head(FORK_NAMESPACE, INDEX_REPO, branch_name(package)) == head_before, (
        "an unchanged run must not advance the branch"
    )


# ── the stale-fork guard ──────────────────────────────────────────────────


def test_a_spent_branch_is_rebuilt_on_the_upstream_head_not_the_forks_own(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A spent announce branch is rebuilt from the **upstream** default branch.

    The trap this guards: a long-lived fork's own `main` drifts months behind the
    index. Basing a rebuilt branch on it would re-propose content the index
    already merged, and the merge request would conflict on the very file every
    announce edits. The proof is the new commit's parent — it must be the
    upstream head, and the fork's stale `main` is deliberately moved somewhere
    else so the two cannot be confused.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    announce(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab")

    # The merge lands upstream, and the announce branch keeps the now-merged
    # commits: from the branch's side this is indistinguishable from unmerged
    # work, which is the whole reason ancestry is asked rather than existence.
    fake_forge.seed_files(INDEX_OWNER, INDEX_REPO, {f"p/{package}.json": _committed_bytes(fake_forge, package)})
    fake_forge.gitlab_close_merge_request(INDEX_FULL, FORK_FULL, branch_name(package))
    fake_forge.seed_branch_at(
        FORK_NAMESPACE, INDEX_REPO, branch_name(package),
        source_owner=INDEX_OWNER, source_repo=INDEX_REPO,
    )
    # The fork's own default branch is left far behind on purpose.
    stale_fork_main = fake_forge.branch_head(FORK_NAMESPACE, INDEX_REPO, "main")
    upstream_main = fake_forge.branch_head(INDEX_OWNER, INDEX_REPO, "main")
    assert stale_fork_main != upstream_main, "precondition: the fork's main must be behind the upstream"

    make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False, cascade=False)
    announce(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0,2.0.0", "--fork", FORK_FULL, forge="gitlab"
    )

    parent = fake_forge.commit_parent(FORK_NAMESPACE, INDEX_REPO, branch_name(package))
    assert parent == upstream_main, (
        "the rebuilt branch must start from the upstream head, not the fork's stale main"
    )


def _committed_bytes(fake_forge: FakeForge, package: str) -> bytes:
    raw = fake_forge.read_file(FORK_NAMESPACE, INDEX_REPO, f"p/{package}.json", branch=branch_name(package))
    assert raw is not None
    return raw


# ── compare-and-swap ──────────────────────────────────────────────────────


def test_a_concurrent_announce_is_unioned_not_clobbered(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """C4 on GitLab: a racing announce that lands first is unioned with, never
    overwritten.

    GitLab has no ref-level compare-and-swap; the guard is `last_commit_id` on
    the root file's update action. The fake advances the branch between the
    client's read and its write, moving the root's last commit and making the
    client's claim stale. The client must re-read the winning head, re-resolve
    its curated universe against it, and retry.

    The curation is `--tags-from-file` (additive union) on purpose. Under
    `--tags` the loser's tag list IS the universe, so a committed tag it does not
    name is dropped by contract and the assertion could not tell a correct drop
    from a clobber. Union makes the winner's tag something that MUST survive.

    A force-push implementation — which is what the donor does here — passes
    nothing in this test: it would land the loser's root wholesale and delete
    `2.0.0`.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False, cascade=False)
    make_package(ocx, unique_repo, "3.0.0", tmp_path, new=False, cascade=False)
    announce(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab")

    # The winner adds 2.0.0 with a placeholder digest, so the loser's retry can
    # be shown to genuinely RE-OBSERVE it rather than copy it forward.
    placeholder = "sha256:" + "0" * 64
    winning_root = json.loads(_committed_bytes(fake_forge, package))
    winning_root["tags"]["2.0.0"] = {"content": placeholder, "observed": "2026-07-24T00:00:00Z"}
    fake_forge.gitlab_concurrent_advance[f"{FORK_FULL}/{branch_name(package)}"] = {
        f"p/{package}.json": json.dumps(winning_root).encode()
    }

    tags_file = tmp_path / "tags.txt"
    tags_file.write_text("3.0.0")
    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags-from-file",
        str(tags_file),
        "--fork",
        FORK_FULL,
        forge="gitlab",
    )

    assert report["status"] == "updated"
    assert not fake_forge.gitlab_concurrent_advance, "the compare-and-swap must have fired"
    final_tags = _fork_root(fake_forge, package)["tags"]
    assert set(final_tags) == {"1.0.0", "2.0.0", "3.0.0"}, (
        "the retry must union against the winning head, never delete its 2.0.0"
    )
    assert final_tags["2.0.0"]["content"] != placeholder, (
        "the concurrently added tag must be genuinely re-observed on the retry"
    )


# ── addressing ────────────────────────────────────────────────────────────


def test_a_nested_group_index_is_addressed_as_one_segment(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A GitLab index in a nested group works, and its path never splits.

    Two things are proven: the announce completes against
    `acme/platform/tooling/index`, and every `/projects/<id>` request carried the
    path percent-encoded as a single segment. A path that split would address a
    different endpoint entirely, and the request log is the only place that is
    visible.
    """
    nested_index = "acme/platform/tooling/index"
    nested_fork = "contrib/team/index"
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    package = f"acme/{unique_repo}"
    fake_forge.seed_root(
        "acme/platform/tooling",
        INDEX_REPO,
        f"p/{package}.json",
        {"name": f"ocx.sh/{package}", "repository": f"oci://{ocx.registry}/{unique_repo}", "tags": {}},
    )
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--index-repo",
        nested_index,
        "--fork",
        nested_fork,
        forge="gitlab",
    )

    assert report["status"] == "updated"
    assert report["fork"] == nested_fork
    project_requests = [path for _, path in fake_forge.requests if path.startswith("/projects/")]
    assert project_requests, "the GitLab surface must have been exercised"
    assert any("acme%2Fplatform%2Ftooling%2Findex" in path for path in project_requests), (
        "the nested index path must travel percent-encoded as one segment"
    )


def test_a_self_hosted_host_requires_an_explicit_forge(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A self-hosted host is refused rather than guessed, and accepted the moment
    the operator declares the forge.

    Both halves matter: the refusal alone could come from any error, so the
    accepting run is what proves the refusal was about the missing declaration.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    out_dir = tmp_path / "out"

    refused = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--index-repo",
        "git.example.com/acme/index",
        "--tags",
        "1.0.0",
        "--out",
        str(out_dir),
        check=False,
    )
    assert refused.returncode != 0, "an undeclared self-hosted forge must be refused"
    assert "--forge" in refused.stderr, "the refusal must name the flag that resolves it"

    # The same coordinate, declared: the run proceeds far enough to prove the
    # forge kind was the only thing missing.
    declared = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--index-repo",
        "git.example.com/acme/index",
        "--tags",
        "1.0.0",
        "--out",
        str(out_dir),
        check=False,
        forge="gitlab",
    )
    assert "--forge" not in declared.stderr, "a declared forge must not re-raise the selection error"


# ── guards ────────────────────────────────────────────────────────────────


def test_a_fork_whose_parent_is_a_stranger_is_refused(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A project at the conventional fork path that is not a fork OF THE UPSTREAM
    is never written to (X5). GitLab answers the parent question with an
    immutable project id, so the guard compares ids, not paths."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    fake_forge.gitlab_seed_project("stranger/index")
    fake_forge.gitlab_fork_parent_override = "stranger/index"

    result = announce(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, check=False, forge="gitlab"
    )

    assert result.returncode != 0, "a stranger project must not become a push target"
    assert fake_forge.read_file(FORK_NAMESPACE, INDEX_REPO, f"p/{package}.json", branch=branch_name(package)) is None, (
        "nothing may be committed to an unverified fork"
    )


def test_a_self_fork_is_refused_by_name(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Forking into the namespace that already owns the index is refused with a
    message naming the fork-free path, instead of failing opaquely inside the
    fork API."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    result = announce(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", INDEX_FULL, check=False, forge="gitlab"
    )

    assert result.returncode != 0
    assert "omit --fork" in result.stderr, "the refusal must point at the path that actually works"


def test_the_fork_free_path_probes_push_access_before_writing(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """Without `--fork`, a credential that cannot push is refused up front and
    nothing is committed."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    fake_forge.no_push_access.add(INDEX_FULL)

    result = announce(ocx, fake_forge, "--package", package, "--tags", "1.0.0", check=False, forge="gitlab")

    assert result.returncode != 0
    assert INDEX_FULL in result.stderr, "the refusal must name the repository"
    assert fake_forge.branch_head(INDEX_OWNER, INDEX_REPO, branch_name(package)) is None, (
        "no branch may be created when the credential cannot push"
    )


def test_the_credential_never_reaches_the_output(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """X6: the announce credential appears in no stream and in no request path.

    The request-path half is not redundant with the output half — a credential
    smuggled into a query string would never print, and would still be logged by
    every proxy between here and the forge.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    result = announce(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab"
    )

    assert TOKEN not in result.stdout + result.stderr, "the credential must not be printed"
    assert not any(TOKEN in path for _, path in fake_forge.requests), "the credential must never enter a URL"
    assert not any(TOKEN in path for _, path in fake_forge.raw_requests), (
        "the credential must never enter a query string either"
    )


def test_a_malformed_compare_is_refused_not_read_as_no_commits(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A compare that comes back without a `commits` array is refused.

    GitLab publishes no ahead/behind verdict, so the client derives one from two
    commit counts. A 200 whose body omits `commits` — an API change, or a proxy
    rewriting the response — would count as zero, and two zeroes read as
    `Identical`, which is precisely the verdict that condemns a LIVE branch to be
    force-rebuilt on the upstream head, discarding unmerged work. The branch head
    is asserted afterwards because "the run failed" alone would not distinguish a
    refusal from a rebuild that then failed for another reason.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    announce(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab")
    head_before = fake_forge.branch_head(FORK_NAMESPACE, INDEX_REPO, branch_name(package))
    assert head_before is not None, "the first announce must have created the branch"

    fake_forge.gitlab_compare_malformed_once = True
    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0,2.0.0",
        "--fork",
        FORK_FULL,
        check=False,
        forge="gitlab",
    )

    assert result.returncode != 0, "a comparison that cannot classify ancestry must fail closed"
    assert not fake_forge.gitlab_compare_malformed_once, "the malformed response must have been served"
    assert fake_forge.branch_head(FORK_NAMESPACE, INDEX_REPO, branch_name(package)) == head_before, (
        "the live branch must not be touched on an unclassifiable comparison"
    )


def test_the_committed_root_is_read_at_a_pinned_commit(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """The root is read at a resolved commit, never through a branch name.

    Reading through a ref name and resolving that same ref to a base SHA in a
    separate call leaves a window: the ref can advance in between, and the commit
    is then based on a head whose version of the root it never saw. On GitLab
    that even passes the `last_commit_id` check, because the check is against the
    newer commit — so the concurrent announce is silently overwritten.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)
    announce(ocx, fake_forge, "--package", package, "--tags", "1.0.0", "--fork", FORK_FULL, forge="gitlab")

    file_reads = [raw for method, raw in fake_forge.raw_requests if method == "GET" and "/repository/files/" in raw]
    assert file_reads, "the root must have been read over the API"
    for raw in file_reads:
        ref = raw.partition("ref=")[2].partition("&")[0]
        assert re.fullmatch(r"[0-9a-f]{40}", ref), f"the root was read at {ref!r}, not at a pinned commit: {raw}"


def test_a_fork_on_another_host_is_refused(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`--fork` naming a different host than `--index-repo` is refused, not
    silently addressed on the index's instance.

    The client is built for the index's host alone, so the fork's host was
    dropped and the fork resolved against the index instance — writing to a
    repository the operator never named."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--index-repo",
        "gitlab.example.com/acme/index",
        "--fork",
        "gitlab.com/forkuser/index",
        check=False,
        forge="gitlab",
    )

    assert result.returncode == 64, f"a cross-host fork is a usage error, got {result.returncode}"
    assert "gitlab.example.com" in result.stderr and "gitlab.com" in result.stderr, (
        "the refusal must name both hosts so the operator can see which one is wrong"
    )


def test_a_coordinate_whose_host_is_not_a_host_is_refused(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`gitlab.com@evil.example/acme/index` never becomes an API base URL.

    As a URL authority that string means userinfo `gitlab.com` at host
    `evil.example`, so accepting it would send `PRIVATE-TOKEN` to
    `evil.example`. It is refused at the parse boundary, and the well-formed
    coordinate beside it still parses — a check that only ever goes red proves
    nothing."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    refused = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--index-repo",
        "gitlab.com@evil.example/acme/index",
        "--out",
        str(tmp_path / "out"),
        check=False,
        forge="gitlab",
    )
    assert refused.returncode == 64, f"a malformed host is a usage error, got {refused.returncode}"

    accepted = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--index-repo",
        f"gitlab.example.com/{INDEX_FULL}",
        "--out",
        str(tmp_path / "out2"),
        check=False,
        forge="gitlab",
    )
    assert accepted.returncode == 0, f"the well-formed coordinate must still work: {accepted.stderr}"


def test_a_nested_namespace_index_is_refused_on_github(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """GitHub organizations do not nest, so a nested `--index-repo` is refused
    before any request — not sent, to come back as a bare 404 that reads as "no
    such repository". The same coordinate is legal on GitLab, which is what makes
    the refusal a forge rule rather than a grammar rule."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--index-repo",
        "acme/platform/index",
        "--out",
        str(tmp_path / "out"),
        check=False,
    )

    assert result.returncode == 64, f"a nested GitHub namespace is a usage error, got {result.returncode}"
    assert "acme/platform" in result.stderr, "the refusal must name the namespace it cannot express"
    assert not any("acme/platform" in path for _, path in fake_forge.requests), (
        "the coordinate must be refused before it reaches the wire"
    )


def test_an_omitted_host_matches_the_canonical_one(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`--index-repo ocx-sh/index --fork github.com/forkuser/index` names ONE
    instance twice and must work.

    The falsifying sibling of `test_a_fork_on_another_host_is_refused`: without
    it, a host-mismatch guard comparing the two `Option<String>` hosts directly
    passes its own test while refusing every ordinary invocation that spells the
    canonical host on one side and omits it on the other.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    report = announce_json(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--fork",
        f"github.com/{FORK_NAMESPACE}/{INDEX_REPO}",
    )

    assert report["status"] == "updated"
    assert _fork_root(fake_forge, package)["tags"].keys() == {"1.0.0"}


def test_a_malformed_command_line_is_diagnosed_before_the_missing_credential(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """A usage error reports as 64 even when no token is set.

    Ordering matters to a human, not just to a table: with the credential check
    first, an operator who typed a self-hosted host without `--forge` is told to
    go and set a token, does so, and only then learns what was actually wrong.
    """
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--index-repo",
        "git.example.com/acme/index",
        token=None,
        check=False,
    )

    assert result.returncode == 64, f"a usage error must outrank the missing credential, got {result.returncode}"
    assert "--forge" in result.stderr

    # ...and the credential check still fires when the command line is fine.
    missing_token = announce(
        ocx, fake_forge, "--package", package, "--tags", "1.0.0", token=None, check=False
    )
    assert missing_token.returncode == 80, f"a missing credential is still 80, got {missing_token.returncode}"


def test_a_path_segment_that_could_retarget_a_request_is_refused(
    ocx: OcxRunner, fake_forge: FakeForge, unique_repo: str, tmp_path: Path
) -> None:
    """`acme?x=1/index` never reaches the wire.

    The GitHub client interpolates the repository path into a URL raw, so a
    segment carrying `?` would address `/repos/acme` with the rest as a query
    string -- a different endpoint than the one named."""
    package = _prepare(ocx, fake_forge, unique_repo, tmp_path)

    result = announce(
        ocx,
        fake_forge,
        "--package",
        package,
        "--tags",
        "1.0.0",
        "--index-repo",
        "acme?x=1/index",
        "--out",
        str(tmp_path / "out"),
        check=False,
    )

    assert result.returncode == 64, f"a malformed path segment is a usage error, got {result.returncode}"
    assert not any("acme?x=1" in path for _, path in fake_forge.raw_requests), (
        "the coordinate must be refused before it reaches the wire"
    )
