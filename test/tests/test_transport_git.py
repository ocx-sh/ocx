# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The `--transport git` write path, end to end.

Covers S-013…S-018, S-022…S-033, S-039 and S-040 of
`plan_index_claim_command.md`: the blobless clone, the four merge-request push
options, the six branch states, the concurrency and convergence paths, C-063's
three credential rungs, C-035's child-environment allowlist, C-033's temporary
workspace and the two transport-level refusals (a redirect, and a sibling
project on the same host). Everything up to and including the version probe is
`test_package_claim.py`'s (WP-16); everything past it is here.

**Most rows drive `ocx package announce`; the two at the foot drive
`ocx package claim`, because S-013 and S-014 are worded as claims.** Announce's
body is one line (`announce.rs:170`) and one published package serves the whole
module, so every mechanism the transport has is cheapest to observe through it.
The claim body is multi-line markdown, and it reaches the wire at all only
because `GitWorkspace::push` escapes its newlines into the two-character `\n`
sequence GitLab converts back at parse time — `render_push_options` still
refuses every byte outside `0x20..=0x7E`, LF included, so the escape is what
makes the value sendable rather than a widening of the guard.

Four harness facts decide what these rows can assert:

* **The git bridge has its own request log.** `fake_forge.git_http_requests` is
  the transport's; `fake_forge.requests` is REST's, and the two never mix. That
  split is what makes S-026's "zero REST writes on every git failure path"
  falsifiable at all.
* **The recording shim is the only observer of the child.** ocx removes its
  workspace on every path that unwinds, so `.git/config`, the reflog, the child
  argv and the child environment exist only in `GitShim.invocations()`.
  Select an invocation **by identity** — a run makes eleven of them and an index
  moves the moment a hygiene flag is added.
* **The push options travel through a real `git`.** They are read back out of
  the bare repository's own `post-receive` hook, so `PushRecord.options` is what
  the server received rather than what ocx believed it sent.
* **One published package serves the whole module** (`git_package`). The
  registry is session-scoped, so a package pushed once is visible to every test;
  re-publishing per row would cost a `package push` each and prove nothing.
"""
from __future__ import annotations

import base64
import json
import stat
import subprocess
import time
import uuid
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

import pytest
from announce_helpers import (
    FIXED_CLOCK,
    INDEX_FULL,
    INDEX_OWNER,
    INDEX_REPO,
    TOKEN,
    branch_name,
    configure_trusted_hosts,
    git_index_project,
    git_transport_env,
    index_root_bytes,
    registry_host,
)
from fake_forge import FakeForge
from git_http_fixture import (
    INITIAL_BRANCH,
    BranchSeedState,
    FixtureHome,
    build_fixture_home,
    git_environment,
    run_git,
)
from git_shim import GitInvocation, GitShim, install_git_shim

from src.helpers import make_package
from src.runner import OcxRunner

# ── shared constants ──────────────────────────────────────────────────────

#: The API credential every row exports unless it is exercising its absence.
#: Deliberately distinct from every other literal below so an assertion that one
#: value reached a surface cannot be satisfied by another.
API_TOKEN = TOKEN

#: The `OCX_ANNOUNCE_GIT_TOKEN` value — push-ladder rung 1. Distinct from
#: `API_TOKEN` so "overrides the push only" is a two-sided claim.
GIT_TOKEN = "glpat_test_push_only_GIT_TOKEN_VALUE_0987654321"

#: The `CI_JOB_TOKEN` value. Distinct again, so a run that picked the wrong rung
#: is visible in the injected header rather than only in a report field.
JOB_TOKEN = "job_test_ci_JOB_TOKEN_VALUE_5555555555"

#: The publishing project a job token claims to run in (`CI_PROJECT_PATH`).
#: Never the index path, so the allowlist read is a genuine cross-project one.
PUBLISHING_PROJECT = "acme/publisher"

#: A second project on the index's host (S-040). Registered as a git route so
#: its URL is real: a synthetic path could not match the credential's prefix
#: under any implementation, which would make "no header reached it" true by
#: construction.
SIBLING_PROJECT = "ocx-sh/other"

#: `GitPushCredential::DEFAULT_USERNAME` — the HTTP Basic user half.
DEFAULT_PUSH_USERNAME = "gitlab-ci-token"

#: The namespace the two claim rows claim, its physical repository, and the
#: branch `claim_branch` derives from it. Literals rather than a `branch_name`
#: call: the claim prefix is `indexbot-claim-`, a different one from announce's,
#: and a helper shared with announce would agree with a rename of either.
CLAIM_PACKAGE = "acme/widget"
CLAIM_PHYSICAL = "oci://ghcr.io/acme/widget"
CLAIM_BRANCH = "indexbot-claim-acme-widget"

#: The owner the claim rows name, as a `LOGIN:ID` pair the forge can confirm.
CLAIM_OWNER_LOGIN = "alice"
CLAIM_OWNER_ID = 7

#: The four push-option keys C-066 renders, sorted. Asserted as a SET, never as
#: a count: swapping `.description` for
#: `merge_request.merge_when_pipeline_succeeds` keeps the count at four while
#: auto-merging a governance request.
EXPECTED_OPTION_KEYS = [
    "merge_request.create",
    "merge_request.description",
    "merge_request.target",
    "merge_request.title",
]

#: The keys that must never be sent, whatever else changes.
FORBIDDEN_OPTION_KEYS = {
    "merge_request.merge_when_pipeline_succeeds",
    "merge_request.remove_source_branch",
}

#: `git_command.rs`'s `SET` table, name and value. Asserted by name AND value in
#: the recorded child environment: this is the only place C-045's fixed commit
#: identity and C-033's pinned locale are observable end to end.
EXPECTED_SET_ENV = {
    "GIT_TERMINAL_PROMPT": "0",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "ocx",
    "GIT_AUTHOR_EMAIL": "noreply@ocx.sh",
    "GIT_COMMITTER_NAME": "ocx",
    "GIT_COMMITTER_EMAIL": "noreply@ocx.sh",
    "LC_ALL": "C",
    "LANGUAGE": "",
}

#: `git_command.rs`'s `UNIX_PASSTHROUGH`. The subset assertion in
#: `::test_child_env_matches_the_allowlist` is written against this union, and
#: that assertion is the falsifiable half of C-035 — a name added to the
#: production table without being added here reds it.
EXPECTED_PASSTHROUGH = (
    "PATH",
    "HOME",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "GIT_SSL_CAINFO",
    "GIT_SSL_CAPATH",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "TMPDIR",
    "TEMP",
    "TMP",
)

#: `git_command.rs`'s `INJECTED` triple, present only on an invocation carrying
#: an ocx credential.
INJECTED_ENV = ("GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0")

#: The names S-031 requires never to reach the child, each of which this module
#: proves **present in the parent** before asserting its absence.
NEVER_IN_CHILD = (
    "OCX_ANNOUNCE_TOKEN",
    "OCX_ANNOUNCE_GIT_TOKEN",
    "CI_JOB_TOKEN",
    "GIT_CURL_VERBOSE",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_TRACE",
)

#: PEP 538 locale coercion inside the CPython shim can add this key to the
#: recorded environment when the child carries no `LC_ALL`/`LC_CTYPE`/`LANG`
#: (`git_shim.py`'s `GitInvocation` docstring). It appears only under the
#: mutation that deletes the `("LC_ALL", "C")` row from `SET` — which is exactly
#: the mutation `::test_child_locale_is_pinned_to_c` exists to catch. It is
#: exempted here so the subset assertion does not red for the coercion instead of
#: for the widening it is written against; **do not widen this set further** —
#: a failure naming any other key is C-035 losing a name, not a shim artefact.
SHIM_LOCALE_ARTEFACT = frozenset({"LC_CTYPE"})

#: The refusal line the `pre-receive` hook writes, WITHOUT a `remote: ` prefix
#: (`send-pack` adds that itself). GitLab's own wording, quoted from the ADR.
#:
#: It does **not** reach ocx's stderr verbatim: `classify_push_failure` matches
#: it and raises `PushRefused` with the phrase table's own message, so a row
#: asserting this literal in `result.stderr` would red. What reaches stderr is
#: the classified message, and the raw stream survives only for a refusal the
#: table does **not** recognise — which is why the redaction rows below arm an
#: unrecognised line instead.
REFUSAL_LINE = "You are not allowed to push code to this project."

#: The distinguishing substring of C-064's notice
#: (`options/forge_write.rs::PUSH_IDENTITY_NOTICE`). Asserted present in the one
#: state that fires it and absent in its twin, so the pair differs in exactly
#: one environment variable.
NOTICE_NEEDLE = "in a GitLab job"


# ── module fixtures ───────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def git_package(ocx_binary: Path, registry: str, tmp_path_factory: pytest.TempPathFactory) -> tuple[str, str, str]:
    """One published package, two tags, for the whole module.

    Returns `(repo, physical_repository, package)`.

    Module-scoped because the registry is session-scoped: a package pushed once
    is visible to every test's own `OcxRunner`, while `make_package` costs a
    build and a push. Publishing it per row would multiply that by thirty-odd and
    prove nothing about the transport, which is this module's whole subject.

    Two tags rather than one: `::test_spent_branch_carries_tag_delta_forward`
    needs a tag already committed on the branch and a *different* tag to
    announce, so the rebuild's tag map can be compared against both.
    """
    home = tmp_path_factory.mktemp("wp17-publisher-home")
    workdir = tmp_path_factory.mktemp("wp17-publisher-work")
    publisher = OcxRunner(ocx_binary, home, registry)
    repo = f"t_wp17_{uuid.uuid4().hex[:8]}"
    for tag in ("1.0.0", "2.0.0"):
        make_package(publisher, repo, tag, workdir / tag, cascade=False, index=False)
    return repo, f"oci://{registry}/{repo}", f"acme/{repo}"


# ── the runner (duplicated deliberately) ──────────────────────────────────


def announce_over_git(
    ocx: OcxRunner,
    fake_forge: FakeForge,
    package: str,
    shim: GitShim,
    home: FixtureHome,
    *args: str,
    token: str | None = API_TOKEN,
    extra_env: Mapping[str, str] | None = None,
    tags: str | None = "1.0.0",
) -> subprocess.CompletedProcess[str]:
    """One `ocx package announce --forge gitlab --transport git` run.

    A copy of `announce_helpers.announce` rather than a parameter on it, per
    `quality-core.md` § DRY's DAMP carve-out and the WP-16 ruling it follows:
    every row here needs the shim's `PATH`, the fixture `HOME` and a credential
    ladder spelled per row, none of which the four REST-only announce modules
    that share `announce_helpers.py` have any use for. Widening the shared helper
    to carry them would put a git-transport concern in a module whose other
    callers must stay unaware of it.

    `--forge gitlab` on every row without exception: `--transport git` against a
    GitHub forge is refused at `options/forge_write.rs` validation, before the
    version probe and long before any transport work (WP-16's
    `::test_transport_git_on_github_64`).
    """
    env: dict[str, str] = {
        "__OCX_TESTING_FORGE_BASE_URL": fake_forge.base_url,
        "__OCX_TESTING_ANNOUNCE_CLOCK": FIXED_CLOCK,
    }
    if token is not None:
        env["OCX_ANNOUNCE_TOKEN"] = token
    env.update(git_transport_env(shim, home))
    if extra_env:
        env.update(extra_env)
    argv = ["package", "announce", "--forge", "gitlab", "--transport", "git"]
    if tags is not None:
        argv += ["--tags", tags]
    argv += [*args, package]
    return ocx.run(*argv, format="json", check=False, env_overrides=env)


def claim_over_git(
    ocx: OcxRunner,
    fake_forge: FakeForge,
    shim: GitShim,
    home: FixtureHome,
    *args: str,
    token: str | None = API_TOKEN,
    extra_env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """One `ocx package claim --forge gitlab --transport git` run.

    The claim twin of `announce_over_git`, spelled out rather than folded into it
    behind a flag: the two commands share no argv past the transport pair, and a
    row that has to read a parameter to learn which command it drives is the one
    that stops being read (`quality-core.md` § DRY's DAMP carve-out).

    `OCX_DEFAULT_REGISTRY` is pinned because the claim root's `name` is
    `<default registry>/<namespace>/<package>` (C-047) and the harness's own
    registry is not `ocx.sh`.
    """
    env: dict[str, str] = {
        "__OCX_TESTING_FORGE_BASE_URL": fake_forge.base_url,
        "__OCX_TESTING_ANNOUNCE_CLOCK": FIXED_CLOCK,
        "OCX_DEFAULT_REGISTRY": "ocx.sh",
    }
    if token is not None:
        env["OCX_ANNOUNCE_TOKEN"] = token
    env.update(git_transport_env(shim, home))
    if extra_env:
        env.update(extra_env)
    return ocx.run(
        "package", "claim", "--forge", "gitlab", "--transport", "git",
        "--repository", CLAIM_PHYSICAL, "--owner", f"{CLAIM_OWNER_LOGIN}:{CLAIM_OWNER_ID}",
        *args, CLAIM_PACKAGE,
        format="json", check=False, env_overrides=env,
    )


def report(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    """The parsed report of a run this row expects to have succeeded."""
    assert result.returncode == 0, f"the run failed (rc={result.returncode}): {result.stderr}"
    return json.loads(result.stdout)


# ── setup helpers ─────────────────────────────────────────────────────────


def prepare(
    ocx: OcxRunner,
    fake_forge: FakeForge,
    git_package: tuple[str, str, str],
    tmp_path: Path,
    *,
    credential_helper: bool = False,
    post_buffer: int | None = None,
    committed_tags: dict[str, Any] | None = None,
) -> tuple[str, GitShim, FixtureHome]:
    """The state every row starts from: a published package, a git-transport
    index project whose `main` carries the claimed root, a recording `git` shim
    and a fixture `HOME`.

    Returns `(package, shim, home)`.
    """
    _repo, physical, package = git_package
    configure_trusted_hosts(ocx, ocx.registry, [registry_host(ocx.registry)])
    git_index_project(fake_forge, package, physical, tags=committed_tags)
    shim = install_git_shim(tmp_path)
    home = build_fixture_home(
        tmp_path / "fixture-home", credential_helper=credential_helper, post_buffer=post_buffer
    )
    return package, shim, home


def prepare_claim(fake_forge: FakeForge, tmp_path: Path) -> tuple[GitShim, FixtureHome]:
    """The state the two claim rows start from: a git-transport index project
    whose `main` carries **no** claim for `CLAIM_PACKAGE`, the owner the claim
    names, a recording `git` shim and a fixture `HOME`.

    The absence of the root is load-bearing rather than incidental: a committed
    one refuses the run at 65 (`already claimed`) before any transport work, so a
    row that seeded it would measure `test_package_claim.py`'s refusal a second
    time while reading as a success row. No published package either — a claim
    reads the forge and the identifier, never the registry.
    """
    fake_forge.git_create_project(INDEX_FULL)
    fake_forge.git_seed_files(INDEX_FULL, INITIAL_BRANCH, {"README.md": b"the index\n"})
    fake_forge.seed_user(CLAIM_OWNER_LOGIN, CLAIM_OWNER_ID)
    shim = install_git_shim(tmp_path)
    home = build_fixture_home(tmp_path / "fixture-home")
    return shim, home


def job_token_env(**extra: str) -> dict[str, str]:
    """The environment of a GitLab job whose push credential is its job token.

    All three of `GITLAB_CI`, `CI_JOB_TOKEN` and the absence of an explicit
    `OCX_ANNOUNCE_GIT_TOKEN` are conjuncts of C-063's job-token rung
    (`credentials.rs::resolve`), and `job_token_push_applies()` gates the whole
    capability matrix on the resolved push half being that token. A row that
    forgets one measures push-ladder rung 2 while reading as a job-token row, and
    every capability check then reports `skipped` rather than refusing.
    """
    env = {
        "GITLAB_CI": "true",
        "CI_JOB_TOKEN": JOB_TOKEN,
        "CI_PROJECT_PATH": PUBLISHING_PROJECT,
    }
    env.update(extra)
    return env


def admit_publishing_project(fake_forge: FakeForge) -> None:
    """Put `CI_PROJECT_PATH` in the index project's job-token allowlist.

    Every job-token row that must **succeed** needs this: the allowlist read only
    happens on a cross-project push, and an absent key is an EMPTY allowlist —
    which is a miss, and refuses at 86. A row that forgot it would measure
    `::test_allowlist_miss_86_names_both_projects` a second time while reading as
    a success row.
    """
    fake_forge.gitlab_job_token_allowlist[INDEX_FULL] = [PUBLISHING_PROJECT]


# ── selection by identity ─────────────────────────────────────────────────


def invocations_named(shim: GitShim, subcommand: str) -> list[GitInvocation]:
    """Every recorded invocation whose argv carries `subcommand`.

    Selection by identity, never by index: a run makes eleven `git` invocations
    and `invocations()[2]` moves the moment a hygiene flag adds one — the exact
    shape `subsystem-tests.md` § Unfalsifiable Greens forbids.
    """
    return [inv for inv in shim.invocations() if subcommand in inv.argv]


def workspace_invocations(shim: GitShim) -> list[GitInvocation]:
    """Every recorded invocation except the version probe.

    "No `git` process started" cannot be spelled `invocations() == []`: the
    argv-boundary version gate (C-065) resolves and probes `git` **before** the
    forge is constructed, so `git --version` runs on every `--transport git`
    invocation including the ones refused at the preflight. The probe is the
    thing WP-16's version rows assert; what "before any push" means here is that
    no *workspace* invocation followed it.
    """
    return [inv for inv in shim.invocations() if "--version" not in inv.argv]


def one_invocation(shim: GitShim, subcommand: str) -> GitInvocation:
    """The single invocation carrying `subcommand`, asserting there is exactly one."""
    found = invocations_named(shim, subcommand)
    assert len(found) == 1, (
        f"expected exactly one `git {subcommand}` invocation, saw {len(found)}: "
        f"{[inv.argv[1:] for inv in shim.invocations()]}"
    )
    return found[0]


def await_import(fake_forge: FakeForge, branch: str, *, timeout: float = 10.0) -> str:
    """Block until the git bridge has imported `branch` back into the REST graph,
    and return the head both stores then agree on.

    **Every read-back after a push needs this**, and the absence of it is a
    read-after-push race in the fixture rather than anything about ocx. The
    bridge writes the receive-pack response first and calls
    `_git_http_import_all_refs` afterwards (`git_http_fixture.py`, DX-5), so
    `git push` can return — and ocx can exit, and pytest can resume — while the
    import is still running on the server thread. A read that wins that race
    returns the branch's **seeded** bytes, and every control around it passes:
    the run really did report a write and the push really did land from the
    right head.

    The oracle is the two stores agreeing, not a fixed sleep and not the push
    record: `git_import_ref` writes `refs[project][branch]` **last**, after
    every commit and tree is registered, so a REST head equal to the bare
    repository's is proof the tree is readable too. A sleep would be a guess
    that gets slower and still races on a loaded host.

    Observed once in the integration branch's full `task verify` on
    `::test_spent_branch_carries_tag_delta_forward`, whose two controls passed
    while the content assertion read the seeded root.
    """
    deadline = time.monotonic() + timeout
    while True:
        landed = fake_forge.git_head(INDEX_FULL, branch)
        imported = fake_forge.branch_head(INDEX_OWNER, INDEX_REPO, branch)
        if landed is not None and imported == landed:
            return landed
        assert time.monotonic() < deadline, (
            f"the git bridge did not import {branch} within {timeout}s: the bare "
            f"repository holds {landed}, the REST graph holds {imported}"
        )
        time.sleep(0.01)


def receive_pack_posts(fake_forge: FakeForge, project: str = INDEX_FULL) -> list[Any]:
    """Every `POST .../git-receive-pack` recorded against `project`.

    **A rejected push makes none.** `(fetch first)` and `(stale info)` are both
    decided client-side against the `info/refs` advertisement, which is a GET, so
    git never sends the pack — measured against git 2.54.0. Counting POSTs is
    therefore an assertion about pushes that *reached the server*, never about
    push attempts.
    """
    return [
        request
        for request in fake_forge.git_http_requests
        if request.method == "POST" and request.service == "git-receive-pack" and request.project == project
    ]


def decoded_injection(inv: GitInvocation) -> tuple[str, str]:
    """The `user`/`secret` pair the recorded child's `GIT_CONFIG_VALUE_0` carries.

    The value is `Authorization: Basic <base64(user:secret)>`, so this is the
    only route from a recorded environment back to the credential the ladder
    resolved — which is what makes "overrides the push only" and "falls through
    to rung 2" observable rather than inferred from a report field.
    """
    value = inv.env["GIT_CONFIG_VALUE_0"]
    blob = value.removeprefix("Authorization: Basic ")
    assert blob != value, f"the injected header is not HTTP Basic: {value!r}"
    user, _, secret = base64.b64decode(blob).decode().partition(":")
    return user, secret


def adjacent(argv: Sequence[str], flag: str, value: str) -> bool:
    """Whether `argv` carries `flag` immediately followed by `value`.

    A membership test over the flattened argv passes when the two arrive in
    different positions, which is not what `git` parses — so C-068's "every
    invocation, without exception" is checked as a window rather than as two
    independent `in` tests.
    """
    return any(argv[i] == flag and argv[i + 1] == value for i in range(len(argv) - 1))


# ══════════════════════════════════════════════════════════════════════════
# S-013 — the first push: refspecs, options, and the argv hygiene flags
# ══════════════════════════════════════════════════════════════════════════


def test_first_push_absent_branch_one_refspec(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-036/S-013: with no branch on the index, the fetch names **one** refspec
    beyond the base, carries `--filter=blob:none`, and never a `--depth`.

    Renamed from the inventory's `test_first_claim_absent_branch_one_refspec` for
    two reasons, both recorded rather than assumed. The first is § E's: the named
    row selects into `invocations()` by index, and a run makes eleven `git`
    invocations, so the pick moves the moment a hygiene flag adds one — here the
    fetch is selected by identity. The second is the module docstring's: a claim
    over git cannot reach a fetch at all.

    Three clauses, because C-036 has three and a refspec count sees none of the
    others: an unconditional second refspec makes a first push impossible (`git
    fetch` fails the *whole* invocation on a source the remote lacks), a shallow
    clone lies about ahead/behind, and without the blob filter the clone is
    unaffordable at index scale.

    Mutations, each reding one clause: drop the `if let Some(branch_refspec)`
    guard in `fetch_refs`; delete `--filter=blob:none`; add `--depth=1`.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = announce_over_git(ocx, fake_forge, package, shim, home)

    assert report(result)["status"] == "updated"
    fetch = one_invocation(shim, "fetch")
    refspecs = [argument for argument in fetch.argv if ":refs/remotes/o/" in argument]
    assert refspecs == [f"{INITIAL_BRANCH}:refs/remotes/o/{INITIAL_BRANCH}"], (
        "a first push has no branch to fetch, so the base refspec is the only one: " f"{fetch.argv[1:]}"
    )
    assert "--filter=blob:none" in fetch.argv, f"the clone must be blobless: {fetch.argv[1:]}"
    assert not [a for a in fetch.argv if a.startswith("--depth")], (
        f"a shallow clone lies about ahead/behind: {fetch.argv[1:]}"
    )


def test_write_tree_survives_siblings_the_blobless_fetch_never_pulled(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """#428: a root whose `p/<ns>/` subtree holds other entries still commits.

    The reported failure, end to end. `update-index` invalidates the cache-tree
    of every directory up to the root, so `write-tree` rebuilds those trees and
    re-verifies each entry under them — including the neighbours that came from
    `read-tree <base>` rather than from `hash-object -w`. The clone is blobless by
    design (C-036, the row above), so those blobs are promisor objects it does not
    hold, and git resolves a missing object by fetching it: a network dial from a
    *local* plumbing command, which carries no credential by design. Against the
    reporter's GitLab that dial was answered 401 and surfaced as
    `the git remote rejected the credential` — exit 80, naming a token that was
    fine.

    The neighbours are seeded rather than assumed: `git_index_project` writes one
    file, and a `p/<ns>/` holding only the entry being rewritten re-verifies
    nothing, which would make this row green with the fix reverted. The blobless
    assertion is the other half of the premise — together they are the state the
    report describes.

    **`git_http_private` is the third**, and without it the sentence above is
    about a run this row never made: the bridge serves `upload-pack` to anyone
    unless it is set (`git_http_fixture.py`), so the promisor dial would be
    answered rather than refused and no 401 would exist to reproduce. It is the
    read half that has to close, not the write half — the dial is a fetch.

    Two guards defend one property here, and the measured reds say which is
    catching what. Dropping `--missing-ok` alone reds at exit 1 on
    `could not fetch <oid> from promisor remote` — the *other* guard, refusing
    the dial. Dropping both reds at exit **80** on a 401, which is the reported
    failure verbatim. Making the project public as well turns that same doubly
    mutated build green on the run and leaves only the two flag assertions to
    red, which is what this row measured before `git_http_private` was set.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.git_http_private = True
    namespace = package.split("/")[0]
    fake_forge.git_seed_files(
        INDEX_FULL,
        INITIAL_BRANCH,
        {
            f"p/{namespace}/neighbour.json": b'{"name":"ocx.sh/acme/neighbour"}\n',
            f"p/{package}/o/sha256/{'a' * 64}.json": b'{"schemaVersion":2}\n',
        },
    )

    result = announce_over_git(ocx, fake_forge, package, shim, home)

    assert report(result)["status"] == "updated"
    fetch = one_invocation(shim, "fetch")
    assert "--filter=blob:none" in fetch.argv, (
        "the premise: without the filter the neighbours' blobs are present and "
        f"write-tree verifies them locally, proving nothing: {fetch.argv[1:]}"
    )
    write_tree = one_invocation(shim, "write-tree")
    assert "--missing-ok" in write_tree.argv, (
        f"write-tree must tolerate the promisor objects: {write_tree.argv[1:]}"
    )
    assert "GIT_NO_LAZY_FETCH" in write_tree.env, (
        "and the lane must refuse the fetch outright, so a future miss fails as "
        f"`invalid object` rather than as somebody's expired token: {write_tree.env}"
    )


def test_two_refspec_form_fails_without_branch(
    fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """The **red control** for the row above, driven through real `git` rather
    than through ocx.

    ocx never emits the unconditional two-refspec form, so this claim cannot be
    made as an ocx assertion — it would be unreachable, and a later reader
    "upgrading" it into one would quietly delete the only evidence that the guard
    in `fetch_refs` is load-bearing. What is proved here is git's own behaviour:
    a fetch naming a refspec the remote does not have fails the **whole**
    invocation, base refspec included.

    Its own falsification is `git fetch` starting to tolerate a missing refspec
    source, which would red it.
    """
    _repo, physical, package = git_package
    git_index_project(fake_forge, package, physical)
    home = build_fixture_home(tmp_path / "fixture-home")
    branch = branch_name(package)
    url = fake_forge.git_url(INDEX_FULL)

    run_git("init", ".", home=home.path, cwd=tmp_path)
    both = run_git(
        "fetch",
        "--filter=blob:none",
        url,
        f"{INITIAL_BRANCH}:refs/remotes/o/{INITIAL_BRANCH}",
        f"{branch}:refs/remotes/o/{branch}",
        home=home.path,
        cwd=tmp_path,
        check=False,
    )
    assert both.returncode != 0, (
        "the unconditional two-refspec form must fail when the branch is absent; "
        f"it succeeded: {both.stdout}\n{both.stderr}"
    )
    only_base = run_git(
        "fetch",
        "--filter=blob:none",
        url,
        f"{INITIAL_BRANCH}:refs/remotes/o/{INITIAL_BRANCH}",
        home=home.path,
        cwd=tmp_path,
        check=False,
    )
    assert only_base.returncode == 0, (
        "the one-refspec form is what makes a first push possible, and it must "
        f"succeed against the same remote: {only_base.stderr}"
    )


def test_push_carries_exactly_the_four_option_keys(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-013/C-066: the push delivers exactly the four merge-request option keys.

    Asserted as the sorted KEY SET, split on the first `=`, plus
    `option_count == "4"`. A bare count of four passes for a substituted key,
    which is the defect the set exists to catch: swapping `.description` for
    `merge_request.merge_when_pipeline_succeeds` keeps the count and auto-merges
    a governance request.

    **Nothing is asserted about `option_count_present`** (DX-19a). git 2.54.0
    exports `GIT_PUSH_OPTION_COUNT=0` to a receive hook whether or not the client
    negotiated push options, so `False` is a state no implementation can reach
    and the only one satisfying an absence assertion is a hook that fabricates
    it. `PushRecord`'s own docstring forbids the assertion by name.

    Mutation: substitute `merge_request.merge_when_pipeline_succeeds` for
    `.description` in `render_push_options` — the count stays 4 and this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    report(announce_over_git(ocx, fake_forge, package, shim, home))

    pushes = fake_forge.git_pushes(INDEX_FULL)
    assert len(pushes) == 1, f"exactly one push: {pushes}"
    keys = sorted(option.split("=", 1)[0] for option in pushes[0].options)
    assert keys == EXPECTED_OPTION_KEYS, f"the option key set is the contract: {pushes[0].options}"
    assert pushes[0].option_count == "4", (
        f"four options were negotiated, and the hook counted {pushes[0].option_count!r}"
    )


def test_merge_when_pipeline_succeeds_is_never_sent(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-013/C-066: the two auto-merge options are refused **server-side**.

    Re-reading `PushRecord.options` here would be
    `::test_push_carries_exactly_the_four_option_keys` counted twice. The
    discriminating form is the fixture's own `pre-receive` hook declining on a
    key set: the ordinary run must **not** be declined.

    Both halves are load-bearing. `git_http_forbidden_push_options` is inert
    without `git_http_pre_receive_refusal` — the hook's predicate is
    `refusal is not None and (not forbidden or keys & forbidden)` — so a row
    arming only the option set declines nothing and reads as a pass. And without
    the second half below, "ocx did not send it" is indistinguishable from "the
    hook declines everything".

    Mutations: render either forbidden key — the first half reds. Arm the
    forbidden set with a key ocx **does** send — the second half reds, which is
    the positive control that the hook can decline at all.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.git_http_pre_receive_refusal = REFUSAL_LINE
    fake_forge.git_http_forbidden_push_options = set(FORBIDDEN_OPTION_KEYS)

    result = announce_over_git(ocx, fake_forge, package, shim, home)

    assert result.returncode == 0, (
        "the hook declines only on the auto-merge keys, and ocx sends neither: " f"{result.stderr}"
    )
    assert len(fake_forge.git_pushes(INDEX_FULL)) == 1, "the push was accepted"

    # The control: the same hook, armed with a key ocx DOES send, declines. The
    # open merge request is closed first, or the second run takes C-042's path 2
    # and never pushes — a control that pushed nothing would prove nothing.
    fake_forge.git_http_forbidden_push_options = {"merge_request.title"}
    fake_forge.gitlab_close_merge_request(INDEX_FULL, INDEX_FULL, branch_name(package))
    second_shim = install_git_shim(tmp_path, subdirectory="git_shim_bin_2")
    declined = announce_over_git(ocx, fake_forge, package, second_shim, home)
    assert declined.returncode == 77, (
        "the hook must be able to decline, or the negative half above proves "
        f"nothing; a declined push is an ordinary refusal: {declined.stderr}"
    )
    assert len(fake_forge.git_pushes(INDEX_FULL)) == 1, (
        "and the declined push must not have been recorded — `pre-receive` writes "
        "no record, so the log still holds only the accepted one"
    )


def test_push_option_values_carry_no_markdown_or_mention(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-067 on the **push-option** surface: `.target` is the index base ref, and
    neither `.title` nor `.description` carries an `@`, a markdown link, or any
    operator-supplied text.

    C-067's absence is asserted at unit scope over the REST `request_body`; the
    push-option pkt-line is a *second* rendering of the same strings, built by
    `render_push_options`, and nothing else asserts the absence there. A
    disclaimer reaching `.description` is markdown into a governance artefact
    humans review, and an `@login` fires a mention in that repository.

    Mutation: interpolate any operator string into the `body` argument
    `publish_over_git` passes to `push` — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    report(announce_over_git(ocx, fake_forge, package, shim, home))

    options = dict(option.split("=", 1) for option in fake_forge.git_pushes(INDEX_FULL)[0].options if "=" in option)
    assert options["merge_request.target"] == INITIAL_BRANCH, (
        f"the request targets the index base ref: {options}"
    )
    for key in ("merge_request.title", "merge_request.description"):
        value = options[key]
        assert "@" not in value, f"{key} must fire no mention: {value!r}"
        assert "](" not in value and "http" not in value, f"{key} must carry no markdown link: {value!r}"
        assert package in value, f"{key} must name the package it is about: {value!r}"


def test_every_invocation_carries_no_redirects(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-068: **every** recorded invocation carries `-c http.followRedirects=false`
    — the local plumbing ones included.

    Asserted as an adjacent `["-c", "http.followRedirects=false"]` window, not as
    a membership test on the flattened argv: the two arriving in different
    positions is not what git parses. The unit row covers the same property
    against its own fixture argv; this is the only place a **real** argv is
    observed, and `--version` is excluded by name because the probe runs before
    any workspace exists.

    Mutation: move the flag from `argv_with` into `network_argv` — every local
    invocation reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    report(announce_over_git(ocx, fake_forge, package, shim, home))

    workspace = [inv for inv in shim.invocations() if "--version" not in inv.argv]
    assert len(workspace) >= 5, f"a push makes several invocations; saw {len(workspace)}"
    for inv in workspace:
        assert adjacent(inv.argv, "-c", "http.followRedirects=false"), (
            f"C-068 admits no exception, and this invocation has none: {inv.argv[1:]}"
        )


def test_tempdir_mode_0700_unix_only(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-033: the temporary workspace is `0700`, read at the earliest moment it
    is observable.

    The shim stats the directory **at entry**, before the child could change it
    and long before ocx removes it. The residual is real and recorded rather than
    hidden: `tempfile` creates the directory with plain `fs::create_dir`
    (`0777 & ~umask`, so `0755` under the usual `022`), which is what makes the
    explicit `set_permissions` load-bearing — but under a `077` umask the
    mutation produces `0700` anyway and this row cannot see it.

    Mutation: delete the `#[cfg(unix)] set_permissions(0o700)` block in
    `GitWorkspace::open` — reds under any umask other than `077`.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    report(announce_over_git(ocx, fake_forge, package, shim, home))

    init = one_invocation(shim, "init")
    assert init.dir_mode is not None, "the shim reads the mode at entry; a None means it could not stat"
    assert stat.S_IMODE(init.dir_mode) == 0o700, (
        f"the workspace must be private: {oct(stat.S_IMODE(init.dir_mode))}"
    )


# ══════════════════════════════════════════════════════════════════════════
# C-051 / S-024 / S-025 — the branch states
# ══════════════════════════════════════════════════════════════════════════


@pytest.mark.parametrize(
    ("state", "expect_lease"),
    [
        # A branch that does not exist is CREATED, and a create has nothing to
        # lease against.
        (BranchSeedState.ABSENT, False),
        # A branch level with the base carries nothing of its own, so the run
        # rebuilds it — and a rebuild is repointed under a lease (C-040).
        (BranchSeedState.IDENTICAL, True),
        # Ahead of the base, whatever its tree: the branch carries commits the
        # base does not, so the run ACCUMULATES and the update is a
        # fast-forward.
        (BranchSeedState.AHEAD_IDENTICAL, False),
        (BranchSeedState.AHEAD_DIFFERING, False),
        # Behind or diverged is a spent branch: rebuilt on the base, leased.
        (BranchSeedState.BEHIND, True),
        (BranchSeedState.DIVERGED, True),
    ],
)
def test_branch_state_drives_the_ref_update_shape(
    ocx: OcxRunner,
    fake_forge: FakeForge,
    git_package: tuple[str, str, str],
    tmp_path: Path,
    state: BranchSeedState,
    expect_lease: bool,
) -> None:
    """C-051/C-040: all six `BranchSeedState` values, under `--transport git`.

    Three named rows cannot see this: they name three of the six by scenario, and
    a `Behind` that pushed and an `AHEAD_DIFFERING` that did not reset are
    exactly the two the mandatory hunt requires. `git_seed_branch` exists to
    reach them.

    Asserted per state: the run converges, a push happened, the `RefUpdate` shape
    visible as `updates[0].old` (`0`×40 for a create, the head the clone was
    taken at otherwise), **and whether the push carried a lease**.

    The lease clause is what makes the parametrisation discriminate, and it was
    added after measurement rather than by design: with only the `updates[0].old`
    assertion, DX-36's `compare` inversion — mapping `(_, 0)` to `Ahead` — leaves
    every one of the six rows **green**, because the head the clone was taken at
    is the same whichever way the comparison reads. `--force-with-lease` is the
    observable that flips: an accumulate carries none and a rebuild carries one,
    so the inversion swaps three rows and reds them. Recorded because a mutation
    that fails to red means "I have not found every guard yet", not "the guard is
    weak".

    Mutations: invert `compare`'s `(0, _)` / `(_, 0)` arms — `IDENTICAL`,
    `BEHIND` and the two `AHEAD_*` rows red on the lease; read `base.sha`
    unconditionally as the commit parent — the accumulating states lose their
    delta and `::test_spent_branch_carries_tag_delta_forward` reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    branch = branch_name(package)
    seeded = fake_forge.git_seed_branch(INDEX_FULL, branch, state)

    result = announce_over_git(ocx, fake_forge, package, shim, home)

    assert result.returncode == 0, f"{state} must still converge: {result.stderr}"
    pushes = fake_forge.git_pushes(INDEX_FULL)
    assert len(pushes) == 1, f"{state}: exactly one push: {pushes}"
    update = pushes[0].updates[0]
    assert update.ref == f"refs/heads/{branch}", f"{state}: the push moves the announce branch: {update}"
    if state is BranchSeedState.ABSENT:
        assert set(update.old) == {"0"}, f"an absent branch is CREATED: {update}"
    else:
        assert update.old == seeded, (
            f"{state}: the update names the head the clone was taken at, not the current one: {update}"
        )
    push_argv = one_invocation(shim, "push").argv
    leased = [argument for argument in push_argv if argument.startswith("--force-with-lease=")]
    assert bool(leased) == expect_lease, (
        f"{state}: expected a lease={expect_lease}, and the push carried {leased or 'none'}"
    )
    if expect_lease:
        assert leased == [f"--force-with-lease={branch}:{seeded}"], (
            f"{state}: the lease names the head the clone was READ at: {leased}"
        )


def test_unchanged_path_with_open_request_performs_no_push(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-024/C-042: an unchanged announce with a merge request already open
    performs **no write of any kind** — the clone it opens is read-only.

    Three absences and two presences, because an absence over an unconstrained
    log is vacuous in both directions: a run that failed before contacting
    anything satisfies every "nothing happened" clause. The presences are what
    say path 2 was taken — the REST merge-request read *did* happen, and the
    returned URL is the one the first run opened.

    **The measured claim is narrower than `publish_over_git`'s doc comment, and
    that is recorded rather than papered over.** That comment says "no clone is
    even created", which is true *of `publish_over_git`* — but announce's own
    pipeline calls `commit_files` first, and under `git` comparing the branch
    needs the clone. So the second run really does `init`, `fetch` and
    `rev-list`, and stops there. What is asserted is what S-024 contracts: **no
    push at all**, no receive-pack request, and no ref-moving invocation
    (`commit-tree`, `update-ref`, `push`) anywhere in the run.

    Mutation: delete the `matches!(pending, PendingCommit::None) && let
    Some(existing)` early return in `publish_over_git` — the run then refreshes
    and pushes, and the `push` assertion reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    first = report(announce_over_git(ocx, fake_forge, package, shim, home))
    opened_url = first["pull_request_url"]
    assert opened_url, "the first run must open the request this row then reuses"

    second_shim = install_git_shim(tmp_path, subdirectory="git_shim_bin_2")
    before = len(fake_forge.git_http_requests)
    second = report(announce_over_git(ocx, fake_forge, package, second_shim, home))

    assert second["pull_request_url"] == opened_url, "the open request is REUSED, never re-minted"
    assert workspace_invocations(second_shim), (
        "the second run must have opened its comparison clone, or the absences "
        "below describe a run that never started"
    )
    for writing in ("push", "commit-tree", "update-ref", "write-tree"):
        assert not invocations_named(second_shim, writing), (
            f"an unchanged run with an open request writes nothing, and it ran `git {writing}`: "
            f"{[i.argv[1:] for i in second_shim.invocations()]}"
        )
    assert len(fake_forge.git_pushes(INDEX_FULL)) == 1, "the second run pushed nothing"
    assert [r for r in fake_forge.git_http_requests[before:] if r.service == "git-receive-pack"] == [], (
        f"and reached no receive-pack route: {fake_forge.git_http_requests[before:]}"
    )
    assert fake_forge.request_count("GET", f"/projects/{fake_forge.gl_project_id(INDEX_FULL)}/merge_requests") >= 1, (
        "the REST merge-request read is what makes the three absences a check "
        f"rather than a description of a run that did nothing: {fake_forge.requests}"
    )


def test_unchanged_path_without_open_request_makes_refresh_commit(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-024/C-042 path 3: with content committed but no request open, the ref is
    made to advance with a refresh commit and pushed.

    "A push happened" alone passes for a push carrying new content, which is the
    opposite of a refresh. The discriminating pair is that the push **advanced**
    the ref the branch already carried, and that the **tree is unchanged** — same
    bytes, new committer timestamp. A server processes push options only for a
    push that genuinely moves a ref, which is why the refresh exists at all.

    Mutations: delete the `refresh_commit` call in the `PendingCommit::None` arm
    — no push at all, and this reds; make `refresh_commit` alter a file — the
    tree-equality half reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    branch = branch_name(package)

    report(announce_over_git(ocx, fake_forge, package, shim, home))
    seeded_head = await_import(fake_forge, branch)
    committed = fake_forge.read_file(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", branch=branch)
    assert committed is not None, "the first run must have committed the root this row refreshes"

    # Close the request the first push opened, so path 3 is what the rerun takes.
    fake_forge.gitlab_close_merge_request(INDEX_FULL, INDEX_FULL, branch)
    second_shim = install_git_shim(tmp_path, subdirectory="git_shim_bin_2")

    report(announce_over_git(ocx, fake_forge, package, second_shim, home))

    pushes = fake_forge.git_pushes(INDEX_FULL)
    assert len(pushes) == 2, f"the refresh push is the second one: {pushes}"
    assert pushes[1].updates[0].old == seeded_head, (
        f"the refresh advances the ref the first push left: {pushes[1].updates[0]}"
    )
    assert pushes[1].updates[0].new != seeded_head, "a refresh that did not move the ref opens nothing"
    await_import(fake_forge, branch)
    assert fake_forge.read_file(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", branch=branch) == committed, (
        "a refresh commit carries the SAME tree — only the committer date moves"
    )


def test_spent_branch_carries_tag_delta_forward(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-025: a diverged (spent) branch is rebuilt on the index base **with the
    tag delta carried forward** — the #399 guarantee — and repointed with a lease
    naming the sha the clone was taken at.

    A lease-presence assertion alone passes for a rebuild that dropped every tag,
    so the content half is the check: both the tag already committed on the
    branch and the newly announced one must survive into the rebuilt root. And
    the lease's sha is read from `git_seed_branch`'s **return value** rather than
    from the argv, which is what makes the comparison a compare rather than an
    echo.

    Mutations: read `base.sha` unconditionally as the parent — the delta is
    dropped and the tags half reds; re-read the head at push time instead of
    using `GitHalf.branch_head` (DX-50) — the lease names the current sha and the
    argv half reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    branch = branch_name(package)
    # The branch already carries a curated `1.0.0`, and has diverged from a base
    # that moved under it.
    branch_root = index_root_bytes(package, git_package[1], {"1.0.0": {"digest": "sha256:" + "0" * 64}})
    head = fake_forge.git_seed_branch(
        INDEX_FULL, branch, BranchSeedState.DIVERGED, files={f"p/{package}.json": branch_root}
    )

    parsed = report(announce_over_git(ocx, fake_forge, package, shim, home, tags="1.0.0,2.0.0"))

    # The two controls that keep the content assertion from reading a root
    # nothing wrote: the run must report a write, and a push must have landed.
    # Without them a run that reported `unchanged` and committed nothing passes
    # by reading back the fixture's own seeded bytes.
    assert parsed["status"] == "updated", f"the rebuild must be a write: {parsed}"
    assert parsed["reserved_tags_dropped"] == [], f"neither tag may be dropped: {parsed}"
    pushes = fake_forge.git_pushes(INDEX_FULL)
    assert len(pushes) == 1 and pushes[0].updates[0].old == head, (
        f"the rebuild repoints the branch from the head it was read at: {pushes}"
    )

    await_import(fake_forge, branch)
    rebuilt = json.loads(fake_forge.read_file(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", branch=branch))
    assert sorted(rebuilt["tags"]) == ["1.0.0", "2.0.0"], (
        f"the rebuild carries the whole curated set forward: {rebuilt['tags']}"
    )
    push_invocation = one_invocation(shim, "push")
    assert f"--force-with-lease={branch}:{head}" in push_invocation.argv, (
        "the lease must name the sha the clone was READ at — re-reading it at push "
        f"time degrades the lease into a plain force: {push_invocation.argv[1:]}"
    )


# ══════════════════════════════════════════════════════════════════════════
# S-022 / S-023 — concurrency and convergence
# ══════════════════════════════════════════════════════════════════════════


def test_moved_target_refetch_and_second_push_succeeds(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-023/C-043: a concurrent writer moves the branch between the read and the
    push; the run re-fetches, regenerates against the winner, and the **second
    push succeeds**.

    Convergence, never merely retry. Until wave 6 this row was a strict xfail
    beside an observed-behaviour twin asserting exit 75, because
    `GitLabForge::commit_files` reached the workspace through the `git_half`
    `OnceCell` and so rebuilt inside the clone the losing attempt was taken at:
    `commit-tree` ran twice with the SAME `-p`, both pushes were refused, and the
    run exited 75 — the ADR's own "a retry guaranteed to fail". The fix refreshes
    the one workspace against the observed remote tip before recomputing, so the
    twin's evidence is now the assertion below: the two parents must DIFFER.

    "The run exited 0" would pass for a retry that clobbered the winner, so the
    content is what is checked: the winner's file must still be there alongside
    ocx's root. `git_http_concurrent_advance` fires once and clears, so the
    second attempt faces a stable remote and nothing but ocx's own stale clone
    could make it fail.

    Mutation: delete the `push_attempted` guard's body in
    `GitLabForge::commit_files` — the second `commit-tree` names the first's
    parent again and every clause below reds.

    `await_import` is here for the shape, not because it is load-bearing here:
    injecting a 0.5s delay into `_git_http_import_all_refs` reds its two sibling
    rows and leaves this one green, because `racing-writer.txt` is on the seeded
    branch already and survives an `is not None` read of pre-import bytes. The
    parent-inequality clause above is what carries this row.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    branch = branch_name(package)
    fake_forge.git_seed_files(
        INDEX_FULL, branch, {f"p/{package}.json": index_root_bytes(package, git_package[1])}
    )
    fake_forge.git_http_concurrent_advance[f"{INDEX_FULL}/{branch}"] = {
        "racing-writer.txt": b"a concurrent announce landed here\n"
    }

    report(announce_over_git(ocx, fake_forge, package, shim, home))

    pushes = invocations_named(shim, "push")
    assert len(pushes) == 2, (
        f"the retry must genuinely re-attempt, or this row measures a single push: "
        f"{[push.argv[1:] for push in pushes]}"
    )
    commits = invocations_named(shim, "commit-tree")
    assert len(commits) == 2, f"and it must genuinely rebuild the commit: {len(commits)}"
    parents = [commit.argv[commit.argv.index("-p") + 1] for commit in commits]
    assert parents[0] != parents[1], (
        "the convergence evidence: the retry must re-parent on the head that WON, "
        f"and a repeated parent is the pre-race clone rebuilding itself ({parents})"
    )
    await_import(fake_forge, branch)
    # Corroboration, not evidence, and only downstream of the clause above: as the
    # docstring records, `racing-writer.txt` is on the seeded branch already and
    # survives an `is not None` read whatever the retry parented on. Read these two
    # as "the parent-inequality above did not come at the cost of losing content",
    # never as an independent proof of convergence.
    assert fake_forge.read_file(INDEX_OWNER, INDEX_REPO, "racing-writer.txt", branch=branch) is not None, (
        "the winner's file is gone, so the retry that re-parented above still "
        "clobbered them"
    )
    assert fake_forge.read_file(INDEX_OWNER, INDEX_REPO, f"p/{package}.json", branch=branch) is not None, (
        "and ocx's own root must be there too"
    )


def test_stale_lease_on_a_rebuild_exits_75(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-023: a racing writer against a **leased** rebuild yields `(stale info)`
    → `StaleLease` → **75**, and is a separate row from `(fetch first)`.

    The two refusals share an exit code, so only the error variant — visible in
    the client stderr the classifier reads — separates them, and `git_stderr.rs`
    lists `StaleLease` first *because* it is the more specific signal. One row
    cannot cover both.

    The rebuild direction is what puts a lease on the push: a diverged branch is
    reset, and `--force-with-lease` is what then refuses to clobber a branch that
    moved underneath the clone.

    Mutation: reorder `REFUSAL_NEEDLES` so `(fetch first)` precedes
    `(stale info)` — the run reports the wrong variant and the message reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    branch = branch_name(package)
    fake_forge.git_seed_branch(INDEX_FULL, branch, BranchSeedState.DIVERGED)
    fake_forge.git_http_concurrent_advance[f"{INDEX_FULL}/{branch}"] = {
        "racing-writer.txt": b"a concurrent announce landed here\n"
    }

    result = announce_over_git(ocx, fake_forge, package, shim, home)

    assert result.returncode == 75, f"a lost lease is a retryable race: {result.stderr}"
    assert "lease" in result.stderr.lower(), (
        "the message must name the LEASE, not a plain non-fast-forward — the two "
        f"share exit 75 and nothing else separates them: {result.stderr!r}"
    )


def test_merge_request_unconfirmed_exits_75_and_rerun_converges(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-022: the push lands but no merge request appears within the poll bound →
    **75**; the rerun takes the refresh-commit direction and converges.

    **This row costs ≥30 s of wall clock, and that is accepted rather than
    worked around.** `CONFIRMATION_SCHEDULE` is a `const` (1 s / 30 s / 30 s
    deadline) with no `__OCX_TESTING_*` seam, and adding one is an edit to a file
    outside this package's set. Do not read the duration as a hang, and do not
    shorten it by weakening the deadline assertion — the deadline IS the
    contract.

    "The rerun exited 0" alone passes for a rerun that opened a **second** merge
    request, which is the duplicate S-003 forbids. The convergence triple is:
    two pushes, the second **advancing the ref the first left** (a losing retry
    would re-offer the same commit), and the second push still carrying
    `merge_request.create` — a push that lost its options would open nothing and
    the poll would exhaust again.

    **Measured, and recorded rather than assumed: skipping `refresh_commit` on
    the `PendingCommit::None` arm does NOT red this row.** That arm is never
    reached here — the rerun's `commit_files` mints a genuinely new commit,
    because the committer timestamp has moved past the first run's, so `pending`
    is `Ready` and the refresh path is not taken. The mutation that reds the 75
    half is the fixture's own `git_http_merge_request_delay = 0.0` on the first
    run; the one that reds the convergence half is leaving it at `None` for the
    rerun. Both were driven, both red. `refresh_commit`'s own red is
    `::test_unchanged_path_without_open_request_makes_refresh_commit`, which is
    the row that genuinely reaches that arm.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.git_http_merge_request_delay = None

    first = announce_over_git(ocx, fake_forge, package, shim, home)
    assert first.returncode == 75, f"an unconfirmed request is retryable: {first.stderr}"
    assert "again" in first.stderr.lower() or "rerun" in first.stderr.lower(), (
        f"the message names the rerun as the remedy: {first.stderr!r}"
    )

    fake_forge.git_http_merge_request_delay = 0.0
    second_shim = install_git_shim(tmp_path, subdirectory="git_shim_bin_2")
    second = report(announce_over_git(ocx, fake_forge, package, second_shim, home))

    pushes = fake_forge.git_pushes(INDEX_FULL)
    assert len(pushes) == 2, f"the rerun pushed: {pushes}"
    assert pushes[1].updates[0].old == pushes[0].updates[0].new, (
        "the second push ADVANCES the ref the first left — a losing retry would "
        f"re-offer the same commit: {pushes[0].updates[0]} then {pushes[1].updates[0]}"
    )
    assert "merge_request.create" in pushes[1].options, (
        f"a push that lost its options opens nothing: {pushes[1].options}"
    )
    assert second["pull_request_url"], "the rerun converges on an opened request"
    assert second["pull_request_number"] == 1, (
        f"one request, updated — never a second: {second['pull_request_url']}"
    )


def test_merge_request_confirmed_within_bound(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-022/C-041: a request that becomes visible **after** a delay inside the
    bound is confirmed by the poll.

    A `0.0` delay is answered by `confirm_merge_request`'s pre-loop probe, before
    the first sleep — so a `0.0` fixture proves the poll never polls, which is
    precisely the risk this row exists for. A positive delay inside the bound is
    the only arrangement that exercises the schedule, and `>= 2` merge-request
    reads is what says the loop ran.

    Mutation: delete the `for delay in backoff_delays(…)` loop, keeping the
    pre-loop probe — this reds while a `0.0` row would not.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.git_http_merge_request_delay = 2.0

    result = report(announce_over_git(ocx, fake_forge, package, shim, home))

    assert result["pull_request_url"], "the poll must confirm the request the push asked for"
    reads = fake_forge.request_count("GET", f"/projects/{fake_forge.gl_project_id(INDEX_FULL)}/merge_requests")
    assert reads >= 2, (
        f"a single read is the pre-loop probe answering; the poll never polled: {reads}"
    )


def test_persistent_non_fast_forward_exits_75(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-003's exit-75 cell (DX-87): `Ahead` with a base that keeps moving →
    re-read, regenerate once, then **75**.

    Driven over the REST transport and against `ocx package claim`, because that
    is the scenario the cell names. It lives in this module rather than in
    `test_package_claim.py` because reaching it needed a new **persistently**
    rejecting knob on the shared ref-update handler
    (`concurrent_ref_advance_persists`), and the retry machinery it exercises is
    this module's subject.

    The one-shot `concurrent_ref_advance` cannot reach the cell at all: claim
    regenerates exactly once, and with the entry popped that retry always
    succeeds — so the exit-code assertion would be green in every state.

    Mutation: turn claim's single regeneration into a loop — the run converges
    and this reds. Drop the key from `concurrent_ref_advance_persists` — the
    retry succeeds and this reds, which is the control that the knob is what
    makes the cell reachable.
    """
    package = "acme/widget"
    physical = "oci://ghcr.io/acme/widget"
    branch = f"indexbot-claim-{package.replace('/', '-')}"
    fake_forge.seed_files(INDEX_OWNER, INDEX_REPO, {"README.md": b"the index\n"})
    fake_forge.seed_user("alice", 7)

    def run_claim(*owners: str) -> subprocess.CompletedProcess[str]:
        owner_args = [flag for owner in owners for flag in ("--owner", owner)]
        return ocx.run(
            "package", "claim", "--repository", physical, *owner_args, package,
            format="json", check=False,
            env_overrides={
                "__OCX_TESTING_FORGE_BASE_URL": fake_forge.base_url,
                "__OCX_TESTING_ANNOUNCE_CLOCK": FIXED_CLOCK,
                "OCX_DEFAULT_REGISTRY": "ocx.sh",
                "OCX_ANNOUNCE_TOKEN": API_TOKEN,
            },
        )

    assert run_claim("alice:7").returncode == 0, (
        "the first claim must land, so the rerun has a branch to advance"
    )
    assert fake_forge.branch_head(INDEX_OWNER, INDEX_REPO, branch) is not None, (
        "and it must have created the branch, or the rerun below takes the CREATE "
        "path and never reaches the ref update this row is about"
    )

    # The rerun carries a SECOND owner, so the rendered root genuinely differs
    # and a commit is attempted. A byte-identical rerun reports `unchanged` and
    # never touches the ref at all, which would make the exit-code assertion
    # below green for a reason that has nothing to do with contention.
    fake_forge.seed_user("bob", 8)
    key = f"{INDEX_FULL}/{branch}"
    fake_forge.concurrent_ref_advance_persists.add(key)
    fake_forge.concurrent_ref_advance[key] = {
        f"p/{package}.json": b'{"name": "ocx.sh/acme/widget", "note": "a racing writer won"}\n'
    }

    result = run_claim("alice:7", "bob:8")

    assert result.returncode == 75, (
        "heavy branch contention the single regeneration did not clear is a "
        f"retryable failure, not a crash: rc={result.returncode} {result.stderr}"
    )
    assert key in fake_forge.concurrent_ref_advance, (
        "the control for the knob itself: it stays armed after firing, which is "
        "the whole difference from the one-shot form"
    )


# ══════════════════════════════════════════════════════════════════════════
# S-015…S-018 — the four-case job-token capability matrix
# ══════════════════════════════════════════════════════════════════════════


def test_job_token_push_disabled_86_before_any_push(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-015/C-029: `ci_push_repository_for_job_token_allowed` reading **false**
    refuses at **86 before any push** — before any `git` process at all.

    "Before any push" is provable rather than asserted: `ensure_push_access` runs
    before `commit_files`, and under `git` the clone is opened *inside*
    `commit_files`, so a preflight refusal leaves both the git bridge and the
    shim empty. The control is the identical run with the knob unset, which
    produces both — it is `::test_unknown_preflight_with_successful_push_exits_0`
    below, run against the same fixture.

    Mutation: map `Some(false)` to a proceed — the run reaches the push and reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.gitlab_job_token_push_allowed[INDEX_FULL] = False
    admit_publishing_project(fake_forge)

    result = announce_over_git(
        ocx, fake_forge, package, shim, home, token=None, extra_env=job_token_env()
    )

    assert result.returncode == 86, f"a disabled capability is an administrator's to fix: {result.stderr}"
    envelope = json.loads(result.stdout)
    assert envelope["error"]["kind"] == "forge_capability_unavailable", (
        "cli-contract.md EXIT-10 requires the machine-readable envelope to name the "
        f"category, not only the exit integer: {envelope}"
    )
    assert "Job token permissions" in result.stderr, (
        f"the message must name the setting an administrator changes: {result.stderr!r}"
    )
    assert INDEX_FULL in result.stderr, f"and the project path: {result.stderr!r}"
    assert fake_forge.git_http_requests == [], (
        f"the refusal precedes the transport: {fake_forge.git_http_requests}"
    )
    assert workspace_invocations(shim) == [], (
        "and precedes any workspace git process — the version probe runs before "
        f"the forge exists at all: {[i.argv[1:] for i in workspace_invocations(shim)]}"
    )


def test_allowlist_miss_86_names_both_projects(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-016/C-029: a cross-project push whose publishing project is absent from
    the index project's job-token allowlist refuses at **86**, naming both paths,
    before any push.

    `CI_PROJECT_PATH` is deliberately not the index path: the allowlist read only
    happens when the push is cross-project, so a row whose publishing project IS
    the index measures the same-project fast path instead.

    Mutation: return `Admits` on an empty list — the run pushes and this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.gitlab_job_token_push_allowed[INDEX_FULL] = True
    fake_forge.gitlab_job_token_allowlist[INDEX_FULL] = []

    result = announce_over_git(
        ocx, fake_forge, package, shim, home, token=None, extra_env=job_token_env()
    )

    assert result.returncode == 86, f"an allowlist miss is a capability gate: {result.stderr}"
    assert INDEX_FULL in result.stderr and PUBLISHING_PROJECT in result.stderr, (
        "both project paths must be named — an operator adds one to the other's "
        f"allowlist: {result.stderr!r}"
    )
    assert fake_forge.git_http_requests == [], f"before any push: {fake_forge.git_http_requests}"
    assert workspace_invocations(shim) == [], (
        "and before any workspace git process: "
        f"{[i.argv[1:] for i in workspace_invocations(shim)]}"
    )


def test_two_signal_old_instance_86(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-017/C-044: an instance that **hides** the field plus a push the server
    refuses is the two-signal rule — **86**.

    The field is left unset, which is the third state (`unknown`): the key is
    absent from the project body entirely, as on GitLab < 18.4 or a hidden
    setting. Only that state admits C-044's promotion.

    Paired with `::test_same_line_with_passed_preflight_77` below: same fixture,
    **same refusal string**, one knob different. That pairing is the whole
    promotion proof — either row alone passes for a build that hardcoded its own
    exit code.

    Mutation: promote unconditionally — the 77 row reds. Never promote — this
    row reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    admit_publishing_project(fake_forge)
    fake_forge.git_http_pre_receive_refusal = REFUSAL_LINE

    result = announce_over_git(
        ocx, fake_forge, package, shim, home, token=None, extra_env=job_token_env()
    )

    assert result.returncode == 86, (
        f"unknown preflight + refused push is the capability signal: {result.stderr}"
    )
    assert invocations_named(shim, "push"), (
        "the promotion is a two-SIGNAL rule: the push must genuinely have been "
        "attempted, or this measures the preflight twice"
    )


def test_same_line_with_passed_preflight_77(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-018/C-044: the **same refusal line** with a preflight that reported
    `passed` is an ordinary `PushRefused` — **77**, not 86.

    One knob apart from the row above, and that is the point: with the two rows
    the exit code is decided by the preflight status, which is what C-029 and
    C-044 were split to guarantee. Passing `PushAccess::skipped_all()` at the
    push makes the promotion unreachable and lands every capability refusal on
    77 silently, and this pair is what sees that.

    Mutation: promote on `Passed` as well — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.gitlab_job_token_push_allowed[INDEX_FULL] = True
    admit_publishing_project(fake_forge)
    fake_forge.git_http_pre_receive_refusal = REFUSAL_LINE

    result = announce_over_git(
        ocx, fake_forge, package, shim, home, token=None, extra_env=job_token_env()
    )

    assert result.returncode == 77, (
        "a preflight that PASSED makes the same refusal an ordinary permission "
        f"denial, never a capability gate: rc={result.returncode} {result.stderr}"
    )


def test_unknown_preflight_with_successful_push_exits_0(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-017's second half, and the row that stops "unknown always refuses": the
    field hidden, the push **accepted**, exit **0**.

    Without it the three refusal rows above are satisfied by a build that refuses
    every job-token run, and the capability matrix would prove nothing. This is
    also the positive control for
    `::test_job_token_push_disabled_86_before_any_push`'s two absences: the same
    fixture with the knob unset produces both a git-bridge request log and a shim
    invocation log.

    Mutation: refuse on `unknown` — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    admit_publishing_project(fake_forge)

    result = report(
        announce_over_git(ocx, fake_forge, package, shim, home, token=None, extra_env=job_token_env())
    )

    assert result["push_credential_kind"] == "job-token", (
        f"the matrix is only armed when the PUSH half is the job token: {result}"
    )
    assert fake_forge.git_http_requests, "the control: an unrefused run does reach the transport"
    assert workspace_invocations(shim), "the control: an unrefused run does open a workspace"
    statuses = {check["name"]: check["status"] for check in result["capability_checks"]}
    assert statuses["job-token-push"] == "unknown", (
        f"a hidden field is `unknown`, never `passed` or `failed`: {result['capability_checks']}"
    )


# ══════════════════════════════════════════════════════════════════════════
# S-014, S-027…S-029 — the credential ladder
# ══════════════════════════════════════════════════════════════════════════


def test_job_token_pickup_headers_and_push_user(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-014/C-026/C-045: inside a GitLab job with no ocx variable, `CI_JOB_TOKEN`
    is picked up for **both** halves — and the commit still authors as ocx.

    Four assertions from one run, because each is satisfied by a build that got
    the others right: the REST reads carry `JOB-TOKEN` (not `PRIVATE-TOKEN`); the
    child's `GIT_CONFIG_KEY_0` scopes the header to the index project's own URL
    and its value decodes to `gitlab-ci-token:<CI_JOB_TOKEN>`; the report says
    `job-token`; and the **commit identity** read out of the pushed commit is
    `ocx <noreply@ocx.sh>`.

    The identity half is the one that needs the fixture `HOME`: `HOME` reaches
    the child by design (C-033 allowlists it for proxy and CA settings), and
    `build_fixture_home` writes a deliberately different `user.name`, so asserting
    the argv would only prove the flags were spelled. S-014's other clause —
    "the push authors as the invoking human" — is GitLab-side attribution the
    fixture cannot model; the half ocx controls is this one.

    Mutation: delete `GIT_AUTHOR_NAME`/`GIT_COMMITTER_NAME` from `SET` — git falls
    back to the fixture `HOME`'s identity and the commit half reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    admit_publishing_project(fake_forge)

    result = report(
        announce_over_git(ocx, fake_forge, package, shim, home, token=None, extra_env=job_token_env())
    )

    assert result["credential_kind"] == "job-token", f"the REST half: {result}"
    assert result["push_credential_kind"] == "job-token", f"the push half: {result}"
    headers = [entry for entry in fake_forge.auth_headers if entry]
    assert headers, (
        "the REST fake enforces no authentication — only the git bridge does — so "
        "an empty header log proves nothing"
    )
    assert all("JOB-TOKEN" in entry for entry in headers), (
        f"a job token travels under its own header, never PRIVATE-TOKEN: {headers}"
    )

    push = one_invocation(shim, "push")
    assert push.env["GIT_CONFIG_KEY_0"] == f"http.{fake_forge.git_url(INDEX_FULL)}.extraHeader", (
        f"the header is scoped to the index project's own URL: {push.env['GIT_CONFIG_KEY_0']!r}"
    )
    user, secret = decoded_injection(push)
    assert (user, secret) == (DEFAULT_PUSH_USERNAME, JOB_TOKEN), (
        "the push half must be the job token under GitLab's fixed Basic user"
    )

    identity = run_git(
        "-C", str(fake_forge.git_repository_path(INDEX_FULL)),
        "log", "-1", "--format=%an <%ae>", f"refs/heads/{branch_name(package)}",
        home=fake_forge.git_scratch_home,
    ).stdout.strip()
    assert identity == "ocx <noreply@ocx.sh>", (
        f"C-045 fixes the commit identity; the fixture HOME's would be "
        f"{home.user_name!r}: {identity!r}"
    )


def test_job_token_not_picked_up_outside_gitlab_ci_exits_80(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-063: the identical job environment **without** `GITLAB_CI` does not pick
    the job token up at all, and the write is refused at **80**.

    `resolve` gates the job-token rung on three conjuncts —
    `transport == Git`, a non-empty `CI_JOB_TOKEN`, and `GITLAB_CI` — and each is
    dropped independently by a plausible implementation, so each needs its own
    row. A test that spells the job environment by hand and forgets `GITLAB_CI`
    silently measures push-ladder rung 2 while reading as a job-token row, and
    `job_token_push_applies()` then reports the whole capability matrix as
    `skipped` rather than refusing anything.

    The **exit code is the discriminator** here, and only because the ladder's
    terminal rung yields an empty API credential: with the conjunct dropped this
    same environment authenticates and the run succeeds. WP-15 pins the
    non-emptiness conjunct on this same 80 path; this is the `GITLAB_CI` one.

    The zero-request clause is what names the contract: the REST fake enforces no
    authentication and the git bridge serves a read anonymously, so a run that
    reached either would have been answered rather than refused.

    Renamed from the inventory's `::test_no_helper_invoked_on_injecting_run`
    sibling `::test_helper_invoked_on_step_three_run`, whose named form is
    **structurally unreachable** — see that row's docstring.

    Mutation: drop the `GITLAB_CI` conjunct from `resolve` — the run exits 0 and
    this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = announce_over_git(
        ocx, fake_forge, package, shim, home, token=None,
        extra_env={"CI_JOB_TOKEN": JOB_TOKEN, "CI_PROJECT_PATH": PUBLISHING_PROJECT},
    )

    assert result.returncode == 80, (
        "without GITLAB_CI the job token is not picked up, so the run has no "
        f"credential at all: rc={result.returncode} {result.stderr}"
    )
    assert fake_forge.requests == [], (
        f"and the refusal precedes the forge, which would have answered: {fake_forge.requests}"
    )
    assert workspace_invocations(shim) == [], (
        f"and no workspace was opened: {[i.argv[1:] for i in workspace_invocations(shim)]}"
    )


def test_non_job_token_keeps_private_token(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-026/C-063 rung 2: a plain `OCX_ANNOUNCE_TOKEN` inside a GitLab job keeps
    `PRIVATE-TOKEN` on the reads **and** becomes the push half.

    The name reads as a header row, and the header alone is not the contract: a
    build that injected nothing when the API token is not a job token sends the
    right REST header and the wrong wire credential. The bridge now refuses that
    push, so `report()` would red on the exit code — but on a *rejected* push,
    which is the same verdict a dozen unrelated defects earn. The injected value
    is asserted because rung 2 has no other observer that names it.

    Mutation: make rung 2 conditional on `api_is_job_token` — the push half
    becomes `None` and this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = report(
        announce_over_git(ocx, fake_forge, package, shim, home, extra_env=job_token_env())
    )

    assert result["credential_kind"] == "token", f"an ocx variable wins rung 1 of the API ladder: {result}"
    assert result["push_credential_kind"] == "token", f"and rung 2 of the push ladder: {result}"
    headers = [entry for entry in fake_forge.auth_headers if entry]
    assert headers, "an empty header log proves nothing"
    assert all("PRIVATE-TOKEN" in entry for entry in headers), (
        f"a credential that is not this job's token is a personal one: {headers}"
    )
    user, secret = decoded_injection(one_invocation(shim, "push"))
    assert (user, secret) == (DEFAULT_PUSH_USERNAME, API_TOKEN), (
        "rung 2 hands the push half the resolved API credential"
    )


def test_git_token_overrides_only_push(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-028/C-063: `OCX_ANNOUNCE_GIT_TOKEN` overrides the **push half only**.

    "Overrides" is a two-sided claim, and asserting only the push half passes for
    a build that repointed both. Both literals are distinct and neither appears
    in the other's surface, so each assertion can fail alone.

    Mutations: make `push_secret` unconditionally the API credential — the push
    half reds; make the API credential follow `OCX_ANNOUNCE_GIT_TOKEN` — the REST
    half reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = report(
        announce_over_git(
            ocx, fake_forge, package, shim, home, extra_env={"OCX_ANNOUNCE_GIT_TOKEN": GIT_TOKEN}
        )
    )

    assert result["push_credential_kind"] == "token", (
        "rung 1 and rung 2 share the `token` spelling — `PushCredentialKind` has "
        f"only `job-token`/`token`/`git-helper`, so the report cannot tell them apart: {result}"
    )
    _user, secret = decoded_injection(one_invocation(shim, "push"))
    assert secret == GIT_TOKEN, (
        "which is why the DECODED secret is the check: it is the only surface that "
        "distinguishes the two rungs"
    )
    rest = [entry for entry in fake_forge.auth_headers if entry]
    assert rest, "an empty header log proves nothing"
    values = {value for entry in rest for values in entry.values() for value in values}
    assert API_TOKEN in values, f"the API half keeps its own token: {values}"
    assert GIT_TOKEN not in values, f"and the git variable must not reach the REST reads: {values}"


def test_transport_git_outside_job_with_no_variable_exits_80(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-014's error case: `--transport git` outside a job with no ocx variable is
    refused at **80**, before the forge is constructed.

    The zero-request half is what names the contract. With `require_credential`
    deleted the run does not stop: it opens a workspace, fetches (the bridge
    serves a read anonymously), commits, and only then meets the push gate's 401.
    So an exit-code-only row could still see 80 and call it a pass while the
    refusal it asserts had been deleted — the empty logs are what tell "refused
    before the forge" from "refused by the forge".

    Mutation: delete `require_credential` at the announce boundary — the two
    logs fill and this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = announce_over_git(ocx, fake_forge, package, shim, home, token=None)

    assert result.returncode == 80, f"an unauthenticated write is an auth failure: {result.stderr}"
    assert "OCX_ANNOUNCE_TOKEN" in result.stderr, (
        f"the message names the variable to set: {result.stderr!r}"
    )
    assert fake_forge.requests == [], (
        f"the refusal precedes the forge, and the fake would have answered: {fake_forge.requests}"
    )
    assert fake_forge.git_http_requests == [], f"and the transport: {fake_forge.git_http_requests}"


def test_empty_ocx_token_falls_through_to_job_token(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-063's non-emptiness qualifier, on the **success** path: an exported but
    empty `OCX_ANNOUNCE_TOKEN` inside a job falls through to the job-token rung.

    The failure mode is a run that authenticates **as nobody**:
    `Basic base64("gitlab-ci-token:")` is a well-formed header carrying an empty
    secret. The bridge rejects an empty half by name (`basic_credential`), so this
    now reds twice over — at `report()`, because the push is refused 401, and at
    the decoded secret below. The decoded clause stays: an exit code shared with
    every other rejected push does not say *which* credential was sent, and this
    row is about that. WP-15 pins the same qualifier on the exit-80 path, where
    the refusal precedes any network call; this is the success-path twin.

    Mutation: replace `non_empty(var::OCX_ANNOUNCE_TOKEN)` with `var(...)` — the
    injected header decodes to `gitlab-ci-token:` and this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    admit_publishing_project(fake_forge)

    result = report(
        announce_over_git(
            ocx, fake_forge, package, shim, home, token="", extra_env=job_token_env()
        )
    )

    assert result["push_credential_kind"] == "job-token", f"the empty variable loses its rung: {result}"
    _user, secret = decoded_injection(one_invocation(shim, "push"))
    assert secret == JOB_TOKEN, "an empty rung-1 value must not authenticate as nobody"


def test_empty_git_token_falls_through_to_api_token(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """The same qualifier on the push ladder: an exported but empty
    `OCX_ANNOUNCE_GIT_TOKEN` falls through to rung 2, never injecting
    `Basic base64("gitlab-ci-token:")`.

    That header authenticates as nobody while `-c credential.helper=` suppresses
    the operator's own helpers, which would otherwise have worked — the failure is
    a run that cannot push and cannot fall back.

    Mutation: replace `non_empty(var::OCX_ANNOUNCE_GIT_TOKEN)` with `var(...)` —
    the injected secret is empty and this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = report(
        announce_over_git(
            ocx, fake_forge, package, shim, home, extra_env={"OCX_ANNOUNCE_GIT_TOKEN": ""}
        )
    )

    assert result["push_credential_kind"] == "token", f"rung 1 was empty, so rung 2 answers: {result}"
    _user, secret = decoded_injection(one_invocation(shim, "push"))
    assert secret == API_TOKEN, "the resolved API credential is the push half"


def test_git_username_reaches_the_wire(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """`OCX_ANNOUNCE_GIT_USERNAME` is the user half of the injected Basic blob.

    It has no other end-to-end observer: the username never appears in argv, in
    the report or in any REST header, so a build that ignored the variable would
    be invisible everywhere else.

    Mutation: ignore the variable in `push_username()` — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    report(
        announce_over_git(
            ocx, fake_forge, package, shim, home,
            extra_env={"OCX_ANNOUNCE_GIT_USERNAME": "deploy-bot"},
        )
    )

    user, secret = decoded_injection(one_invocation(shim, "push"))
    assert (user, secret) == ("deploy-bot", API_TOKEN), "the operator's username is the Basic user half"


def test_git_username_carrying_a_colon_is_ignored(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """A `:` in `OCX_ANNOUNCE_GIT_USERNAME` is refused and the default used.

    HTTP Basic partitions `user:secret` on the **first** colon, so a username
    carrying one silently re-partitions the pair and the server reads a different
    secret than ocx resolved — a credential-shaped defect with no other
    acceptance observer.

    Mutation: drop the `:` filter in `push_username()` — the decoded user becomes
    `a` and this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    report(
        announce_over_git(
            ocx, fake_forge, package, shim, home,
            extra_env={"OCX_ANNOUNCE_GIT_USERNAME": "a:b"},
        )
    )

    user, secret = decoded_injection(one_invocation(shim, "push"))
    assert (user, secret) == (DEFAULT_PUSH_USERNAME, API_TOKEN), (
        "a username that would re-partition the pair is refused, and the default used"
    )


def test_no_helper_invoked_on_injecting_run(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-034: a credential-injecting run resets `credential.helper` and invokes
    none.

    Both polarities are asserted **in this row**, because the negative alone is
    true in every state — including with `-c credential.helper=` deleted, since
    with no helper configured git calls none. The fixture `HOME` carries a
    recording helper, and the positive control is a **replay**: the same `HOME`,
    the same URL, driven through `run_git` with no injected config, must record a
    call. Without that replay "no helper was invoked" is equally true of a helper
    git could not execute — a `noexec` `TMPDIR` or a lost `chmod` — and git says
    nothing when that happens.

    The replay is used rather than citing WP-4's
    `::test_credential_helper_records_an_invocation` because it proves *this*
    fixture `HOME`'s helper is live in *this* run, which is the thing the negative
    is about.

    **The inventory's paired row, `::test_helper_invoked_on_step_three_run`, is
    unwritable and is not shipped.** C-063 rung 3 needs `credentials.push()` to be
    `None`, which needs the API credential empty (rung 2 copies it otherwise) —
    and an empty API credential is refused at exit 80 by `require_credential`
    before the forge is constructed, while the `--out` carve-out that exempts it
    is itself refused with `--transport git` at exit 64. So
    `push_credential_kind: "git-helper"` has no reachable state on any writing
    run, and the reachable half of the pair is
    `::test_job_token_not_picked_up_outside_gitlab_ci_exits_80`.

    Mutation: delete `-c credential.helper=` from `network_argv` — the helper is
    invoked and the negative reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path, credential_helper=True)

    report(announce_over_git(ocx, fake_forge, package, shim, home))

    push = one_invocation(shim, "push")
    assert push.env.get("GIT_CONFIG_COUNT") == "1", (
        "the run must be an injecting one, or the negative below is about nothing: "
        f"{push.env.get('GIT_CONFIG_COUNT')!r}"
    )
    assert adjacent(push.argv, "-c", "credential.helper="), (
        f"the reset rides the injecting invocations: {push.argv[1:]}"
    )
    assert home.credential_calls() == [], (
        f"an injected credential must suppress the operator's helper: {home.credential_calls()}"
    )

    # The positive control, in this row rather than cited: `git credential fill`
    # is the smallest thing that invokes a helper, and it must record a call
    # under this very `HOME`. An empty answer above is otherwise
    # indistinguishable from a helper git could not execute — a `noexec` TMPDIR
    # or a lost `chmod` — and git says nothing when that happens. `ls-remote`
    # would not do: the bridge serves a read anonymously unless
    # `git_http_credential` is set, so git is never challenged and never asks.
    subprocess.run(
        ["git", "credential", "fill"],
        input=f"protocol=http\nhost=127.0.0.1\npath={INDEX_FULL}.git\n\n",
        env=git_environment(home.path),
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert home.credential_calls(), (
        "the fixture helper never recorded anything even when git asked for a "
        "credential, so the assertion above measured a broken helper rather than "
        "the reset"
    )


def test_pre_existing_token_warning_emitted(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-027/C-064: inside `GITLAB_CI`, with a non-job `OCX_ANNOUNCE_TOKEN` and no
    git token, one stderr line **before the write** says the push will not
    authenticate as the pipeline.

    C-064's condition is five conjuncts (`credentials.rs::push_is_explicit` is
    the one WP-15 added for it), and the notice names the push **credential
    kind** and the variable it came from — never the HTTP Basic username, which
    in exactly this state is the fixed protocol string `gitlab-ci-token` and
    would name the wrong party (DX-86).

    Mutation: delete the `!credentials.push_is_explicit()` conjunct — the warning
    also fires on the `OCX_ANNOUNCE_GIT_TOKEN` run, which the paired row below
    would not catch; delete the notice — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = announce_over_git(
        ocx, fake_forge, package, shim, home, extra_env=job_token_env()
    )

    assert result.returncode == 0, f"the notice is a diagnostic, never a refusal: {result.stderr}"
    assert NOTICE_NEEDLE in result.stderr, f"the notice must fire in this state: {result.stderr!r}"
    assert "OCX_ANNOUNCE_TOKEN" in result.stderr, f"the notice names the credential: {result.stderr!r}"
    assert "CI_JOB_TOKEN" in result.stderr, f"and what it is NOT: {result.stderr!r}"
    assert DEFAULT_PUSH_USERNAME not in result.stderr, (
        "DX-86: the Basic username is a protocol artefact naming the wrong party, "
        f"and must not be printed as an identity: {result.stderr!r}"
    )
    assert result.stdout.strip().startswith("{"), "the notice is on stderr and stdout stays a clean report"
    assert "OCX_ANNOUNCE_TOKEN" not in result.stdout, f"and never reaches stdout: {result.stdout!r}"


def test_pre_existing_token_warning_absent_outside_gitlab_ci(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-027: the identical run **outside** `GITLAB_CI` emits nothing.

    Without this half the notice could fire on every `--transport git` run and the
    row above would still pass — the whole point is that it fires in the one
    state that surprises. The positive control is that row: the same
    `OCX_ANNOUNCE_TOKEN`, the same transport, one variable different — and the
    push credential kind asserted here is the same `token`, so the two runs
    differ in nothing except `GITLAB_CI`.

    Mutation: drop the `in_gitlab_ci()` conjunct — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = announce_over_git(ocx, fake_forge, package, shim, home)

    parsed = report(result)
    assert parsed["push_credential_kind"] == "token", (
        f"the same rung-2 push half as the row above: {parsed}"
    )
    assert NOTICE_NEEDLE not in result.stderr, (
        f"the notice fires only inside a GitLab job: {result.stderr!r}"
    )


# ══════════════════════════════════════════════════════════════════════════
# S-030…S-033 — the child environment, the secrets, the workspace
# ══════════════════════════════════════════════════════════════════════════


def test_child_env_matches_the_allowlist(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-031/C-035: the child's environment is a **subset** of the allowlist
    tables, carries none of the six forbidden names, and carries every `SET` row
    at its contracted value.

    Three assertions, not one, and the reason is recorded in `git_command.rs`
    itself: asking the built child whether it holds `GIT_TRACE` is green in every
    state of the code, **including with the whole `NEVER` table deleted**, because
    the child is built by construction from `Env::clean()`. The subset assertion
    is the falsifiable half — any widening of a passthrough table reds it. The
    by-name absences each prove the name was **present in the parent** first, or
    they are absences over nothing. The by-name presences are what give C-045 and
    C-033 a red at acceptance scope.

    `LC_CTYPE` is exempted from the subset for PEP 538 locale coercion inside the
    CPython shim — see `SHIM_LOCALE_ARTEFACT`, which records the trap: it appears
    only under the mutation that deletes `("LC_ALL", "C")`, and the failure then
    names `LC_CTYPE` and invites widening the expected set instead of restoring
    the row.

    Mutations: add `CI_JOB_TOKEN` or an `OCX_ANNOUNCE_`-matching name to
    `UNIX_PASSTHROUGH` — the subset and a named absence both red; replace
    `Env::clean()` with `Env::new()` — the subset reds hard.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    parent = {
        "OCX_ANNOUNCE_GIT_TOKEN": GIT_TOKEN,
        "GIT_CURL_VERBOSE": "1",
        "GIT_ASKPASS": "/bin/true",
        "SSH_ASKPASS": "/bin/true",
        "GIT_TRACE": "1",
        **job_token_env(),
    }

    report(announce_over_git(ocx, fake_forge, package, shim, home, extra_env=parent))

    push = one_invocation(shim, "push")
    allowed = set(EXPECTED_PASSTHROUGH) | set(EXPECTED_SET_ENV) | set(INJECTED_ENV) | SHIM_LOCALE_ARTEFACT
    assert set(push.env) <= allowed, (
        "C-035's tables are the whole child environment; these names are outside "
        f"them: {sorted(set(push.env) - allowed)}"
    )
    for name in NEVER_IN_CHILD:
        assert name in parent or name == "OCX_ANNOUNCE_TOKEN", (
            f"{name} must be set in the PARENT, or its absence below is over nothing"
        )
        assert name not in push.env, f"{name} reached the child: {push.env[name]!r}"
    for name, value in EXPECTED_SET_ENV.items():
        assert push.env.get(name) == value, (
            f"{name} must be {value!r} in every child, and is {push.env.get(name)!r}"
        )


def test_ambient_git_trace_family_does_not_reach_child(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-031: `GIT_TRACE2` and `GIT_TRACE_PACKET` are absent too — `NEVER` is a
    **prefix** table, not a name list.

    An exact-name row stays green under a change from prefix matching to exact
    matching, which admits the whole family: `GIT_TRACE_CURL` writes the request
    headers, credential included, to a file of the operator's choosing.

    Each name is proved present in the parent first, and the run's success is what
    says the child was built at all.

    Mutation: change `NEVER` matching from prefix to exact — the family members
    red.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    family = {"GIT_TRACE": "1", "GIT_TRACE2": "1", "GIT_TRACE_PACKET": "1", "GIT_TRACE_CURL": "1"}

    report(announce_over_git(ocx, fake_forge, package, shim, home, extra_env=family))

    push = one_invocation(shim, "push")
    for name in family:
        assert name not in push.env, f"the whole GIT_TRACE family is barred: {name}={push.env[name]!r}"


def test_ambient_http_proxy_passed_through(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-033/C-035: an ambient `http_proxy` **and** `no_proxy` reach the child
    verbatim.

    Passing a proxy through and having the run succeed are different claims —
    `no_proxy` is what keeps the loopback reachable, so this is a green about the
    environment rather than about curl. The proxy value points at a port nothing
    listens on, so a build that forwarded `http_proxy` without `no_proxy` would
    fail the run rather than pass this row silently.

    Mutation: drop `http_proxy` from `UNIX_PASSTHROUGH` — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    proxy = {"http_proxy": "http://127.0.0.1:1", "no_proxy": "127.0.0.1,localhost"}

    report(announce_over_git(ocx, fake_forge, package, shim, home, extra_env=proxy))

    push = one_invocation(shim, "push")
    for name, value in proxy.items():
        assert push.env.get(name) == value, f"{name} must reach the child verbatim: {push.env.get(name)!r}"


def test_ambient_uppercase_https_proxy_passed_through(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-033: the **uppercase** spelling is a distinct name on Unix and is carried
    too.

    `EnvKey` folds case only under `#[cfg(windows)]`, so a table holding only the
    lowercase forms loses the proxy on every runner that exports `HTTPS_PROXY` —
    which is most CI images. The lowercase row above cannot see that.

    Mutation: drop `HTTPS_PROXY` from `UNIX_PASSTHROUGH` — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    proxy = {"HTTPS_PROXY": "http://127.0.0.1:1", "NO_PROXY": "127.0.0.1,localhost"}

    report(announce_over_git(ocx, fake_forge, package, shim, home, extra_env=proxy))

    push = one_invocation(shim, "push")
    for name, value in proxy.items():
        assert push.env.get(name) == value, f"{name} must reach the child verbatim: {push.env.get(name)!r}"


def test_child_locale_is_pinned_to_c(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-032/C-035, renamed from the inventory's
    `test_lc_all_c_keeps_classifier_matching`.

    **The named form has no reachable red.** `LC_ALL` and `LANG` are in neither
    `PASSTHROUGH` table, so the parent's locale can never reach the child;
    deleting the `("LC_ALL", "C")` row from `SET` leaves the child with *no*
    locale variable at all, git defaults to C, and the classifier still matches —
    green in every state of the code. The environment assertion is the falsifiable
    form of the same property, and it also does not depend on a `de_DE.UTF-8`
    locale being generated on the runner, which it usually is not.

    The behavioural clause is kept as corroboration and **explicitly labelled not
    the check**: it stays green under the mutation, which is exactly why it cannot
    be the assertion.

    Mutation: delete `("LC_ALL", "C")` from `SET` — the environment assertion reds
    and the corroborating clause does not.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.git_http_pre_receive_refusal = REFUSAL_LINE
    fake_forge.gitlab_job_token_push_allowed[INDEX_FULL] = True

    result = announce_over_git(
        ocx, fake_forge, package, shim, home,
        extra_env={"LANG": "de_DE.UTF-8", "LC_ALL": "de_DE.UTF-8", "LC_MESSAGES": "de_DE.UTF-8"},
    )

    push = one_invocation(shim, "push")
    assert push.env.get("LC_ALL") == "C", (
        f"the check: the child's locale is pinned regardless of the parent's: {push.env.get('LC_ALL')!r}"
    )
    assert push.env.get("LANGUAGE") == "", f"and LANGUAGE is emptied: {push.env.get('LANGUAGE')!r}"
    assert "LANG" not in push.env, f"LANG is in neither passthrough table: {push.env.get('LANG')!r}"
    # Corroboration, NOT the check: this clause stays green with the SET row
    # deleted, because git then falls back to C anyway.
    assert result.returncode == 77, (
        f"corroboration only — the English classifier phrases still matched: {result.stderr}"
    )


# ── the failure-path matrix ───────────────────────────────────────────────

#: The git failure arms S-030 and S-026 are parametrised over.
#:
#: `(id, arm)` where `arm(fake_forge, branch)` arms the fixture. Each reaches a
#: DIFFERENT point of the transport — an unreadable advertisement, a refused
#: pack, a declined hook, a lost race, an unconfirmed request — so a build that
#: leaked on one path is not hidden by the others passing.
#:
#: The three preflight refusals are **deliberately excluded**: they legitimately
#: spawn no `git` at all, so `::test_tempdir_removed_on_every_failure_path` would
#: pass trivially for them. They are covered by their own rows above, which
#: assert the empty logs as the contract rather than as a side effect.
FAILURE_ARMS: Mapping[str, Any] = {
    "info-refs-403": lambda forge, branch: setattr(forge, "git_http_forbid_info_refs", True),
    "receive-pack-403": lambda forge, branch: setattr(forge, "git_http_forbid_receive_pack", True),
    "pre-receive-declined": lambda forge, branch: setattr(
        forge, "git_http_pre_receive_refusal", REFUSAL_LINE
    ),
    "redirect-receive-pack": lambda forge, branch: setattr(
        forge, "git_http_redirect_receive_pack", f"{forge.git_url(SIBLING_PROJECT)}/git-receive-pack"
    ),
    "non-fast-forward": lambda forge, branch: forge.git_http_concurrent_advance.__setitem__(
        f"{INDEX_FULL}/{branch}", {"racing-writer.txt": b"a concurrent announce landed here\n"}
    ),
    "merge-request-unconfirmed": lambda forge, branch: setattr(
        forge, "git_http_merge_request_delay", None
    ),
}


def arm_failure(fake_forge: FakeForge, arm: str, branch: str) -> None:
    """Arm one failure path, plus whatever the arm needs to reach it."""
    fake_forge.git_create_project(SIBLING_PROJECT)
    if arm == "non-fast-forward":
        fake_forge.git_seed_branch(INDEX_FULL, branch, BranchSeedState.IDENTICAL)
    FAILURE_ARMS[arm](fake_forge, branch)


@pytest.mark.parametrize("arm", sorted(FAILURE_ARMS))
def test_secret_absent_from_every_surface_on_every_failure_path(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path, arm: str
) -> None:
    """S-030/C-022: on **every** git failure path, neither live form of the secret
    appears in argv, in any snapshotted `.git` surface, in stderr or in stdout.

    Three positive controls come first, and they are what make the negative a
    check. The plan's own loop shape — iterate invocations, assert the secret is
    in no snapshot — is vacuous on a whole run when `git_dir` resolved to a
    non-repository, which the shim's docstring warns about; and `logs/HEAD` is
    **absent** in a no-checkout workspace, so a naive `secret not in
    snapshot["logs/HEAD"]` is a negative over an empty set.

    So: (a) invocations were recorded at all, (b) at least one looked at a real
    git directory, (c) the surface keys asserted against are **present** in that
    snapshot — and only then (d) the negatives.

    Two forms, not three: `Injected::secrets` collects the plaintext push secret
    and the base64 `user:secret` blob, and documents why the API credential is
    not a third. On this path git never holds the API token — the `OCX_ANNOUNCE_`
    prefix in `NEVER` keeps it out of the child — so its guarantee is absence,
    which `::test_each_secret_form_proved_red_then_green` asserts.

    Mutation: build `remote` as `https://user:token@host/…` in
    `GitWorkspace::open` — (d) reds on argv and on the reflog snapshot. The
    stderr half is reded separately by
    `::test_each_secret_form_proved_red_then_green`, which arms a refusal the
    phrase table does not recognise: a **recognised** refusal never carries git's
    raw stderr into the message at all, so no mutation of `redact` can red it
    here.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    arm_failure(fake_forge, arm, branch_name(package))

    result = announce_over_git(ocx, fake_forge, package, shim, home)
    assert result.returncode != 0, f"{arm} must fail, or this row measures the happy path: {result.stdout}"

    invocations = shim.invocations()
    assert invocations, f"{arm}: no git ran, so every surface below is empty by construction"
    looked = [inv for inv in invocations if inv.git_dir_exists]
    assert looked, (
        f"{arm}: no invocation resolved a real git directory, so every snapshot is "
        "empty and the negatives are vacuous"
    )
    assert any("config" in inv.snapshot for inv in looked), (
        f"{arm}: `.git/config` — the remote URL surface — was never captured"
    )
    assert any(
        any(key.startswith("logs/refs/") for key in inv.snapshot) for inv in looked
    ), f"{arm}: the reflog was never captured, so the reflog negative is over nothing"

    encoded = base64.b64encode(f"{DEFAULT_PUSH_USERNAME}:{API_TOKEN}".encode()).decode()
    for form in (API_TOKEN, encoded):
        for inv in invocations:
            assert not any(form in argument for argument in inv.argv), (
                f"{arm}: a secret form reached argv: {inv.argv[1:]}"
            )
            for key, blob in inv.snapshot.items():
                assert form.encode() not in blob, f"{arm}: a secret form reached {key}"
        assert form not in result.stderr, f"{arm}: a secret form reached stderr"
        assert form not in result.stdout, f"{arm}: a secret form reached stdout"


def test_api_credential_never_reaches_the_child_or_argv(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-022's **third** form, on a run where it genuinely differs from the push
    secret: the API credential is absent from the child environment and from
    every argv.

    **Renamed from the inventory's `::test_each_secret_form_proved_red_then_green`,
    because that row's central claim has no reachable red at acceptance scope,
    and the rename is the finding.** `Injected::secrets` carries two live forms —
    the plaintext push secret and its base64 `user:secret` blob — and `redact`
    masks them in the stderr that reaches `GitPushFailed`. But *every* refusal
    this fixture can produce is one `git_stderr.rs`'s phrase table
    **recognises**, and a recognised refusal never carries git's raw stderr into
    the error at all: `classify_push_failure` raises `PushRefused` /
    `NonFastForward` / `StaleLease` / `WriteCapabilityUnavailable` with the
    table's own wording. Measured, both ways: a `pre-receive` line carrying both
    secret forms verbatim comes back as `refused by the server: pre-receive hook
    declined`, because `(pre-receive hook declined)` is git's wording for *any*
    hook exiting non-zero and is itself a needle. So no mutation of `redact`
    could turn a stderr assertion red here, and the unit row
    `redactor_masks_all_three_secret_forms` (WP-6) is that guarantee's only
    falsifiable observer.

    What **is** reachable is the third form's guarantee, and it is a different
    kind of claim: on this path git never holds the API credential, because the
    `OCX_ANNOUNCE_` prefix in `NEVER` keeps it out of the child by construction.
    `OCX_ANNOUNCE_GIT_TOKEN` is set to a different value here precisely so the
    two genuinely differ — under rung 2 they are the same string and an absence
    assertion could not tell which one it was about.

    The absence of the two live forms from stderr and stdout is asserted by
    `::test_secret_absent_from_every_surface_on_every_failure_path`, over every
    arm of `FAILURE_ARMS`; it is not repeated here.

    Mutation: add `"OCX_ANNOUNCE_TOKEN"` to `UNIX_PASSTHROUGH` — this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)

    result = report(
        announce_over_git(
            ocx, fake_forge, package, shim, home, extra_env={"OCX_ANNOUNCE_GIT_TOKEN": GIT_TOKEN}
        )
    )

    push = one_invocation(shim, "push")
    _user, secret = decoded_injection(push)
    assert secret == GIT_TOKEN and API_TOKEN != GIT_TOKEN, (
        "the two credentials must genuinely differ on this run, or the absence "
        f"below cannot say which one it is about: {result['push_credential_kind']}"
    )
    assert API_TOKEN not in json.dumps(dict(push.env)), (
        "the API credential is a form git never holds — the OCX_ANNOUNCE_ prefix "
        f"in NEVER keeps it out of the child entirely: {sorted(push.env)}"
    )
    assert not any(API_TOKEN in argument for inv in shim.invocations() for argument in inv.argv), (
        "and it never reaches an argv either"
    )


@pytest.mark.parametrize("arm", sorted(FAILURE_ARMS))
def test_git_failure_records_zero_rest_writes(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path, arm: str
) -> None:
    """S-026: a git failure of any kind produces **no REST write** — and the REST
    **reads** did happen.

    An empty request log satisfies "zero REST writes" and equally satisfies "the
    run never started", so the positive half is the whole difference. S-026 says
    "on every git failure path", and one arm proves one arm — hence the
    parametrisation.

    Mutation: fall back to the REST `commit_files_once` when the git push fails —
    a write appears and this reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    arm_failure(fake_forge, arm, branch_name(package))

    result = announce_over_git(ocx, fake_forge, package, shim, home)
    assert result.returncode != 0, f"{arm} must fail: {result.stdout}"

    assert fake_forge.requests, f"{arm}: the REST reads must have happened, or the negative is vacuous"
    writes = [(method, path) for method, path in fake_forge.requests if method != "GET"]
    assert writes == [], f"{arm}: the git transport never falls back to a REST write: {writes}"


@pytest.mark.parametrize("arm", sorted(FAILURE_ARMS))
def test_tempdir_removed_on_every_failure_path(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path, arm: str
) -> None:
    """C-033/S-026: the temporary clone is removed on every path that unwinds.

    The workspace path is read out of `GitInvocation.cwd` — ocx passes no `-C`,
    and `git_child_command` sets `current_dir(workdir)`. Without the "at least one
    invocation" clause a run that never spawned `git` passes trivially, which is
    why the three preflight arms are excluded from `FAILURE_ARMS` by name rather
    than left to pass silently.

    Mutation: replace the `TempDir` guard with a `PathBuf` and no removal — every
    arm reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    arm_failure(fake_forge, arm, branch_name(package))

    result = announce_over_git(ocx, fake_forge, package, shim, home)
    assert result.returncode != 0, f"{arm} must fail: {result.stdout}"

    invocations = shim.invocations()
    workspace = {inv.cwd for inv in invocations if "--version" not in inv.argv}
    assert workspace, f"{arm}: no workspace invocation was recorded, so the removal below is vacuous"
    for directory in workspace:
        assert not Path(directory).exists(), f"{arm}: the temporary clone survived at {directory}"


# ══════════════════════════════════════════════════════════════════════════
# S-039 / S-040 — redirect refusal and credential scope
# ══════════════════════════════════════════════════════════════════════════


def test_push_does_not_follow_a_redirect(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-039/C-068: the index host answers the receive-pack POST with a redirect
    to a sibling project; the push does **not** follow it and no `Authorization`
    reaches the target.

    The sibling-absence half alone is vacuous: nothing reached the sibling
    because the push failed, which is equally true if the credential was never
    injected at all. The positive control is that the 302'd POST **carried an
    `Authorization` header** — the credential was live at the moment the redirect
    was refused. WP-4's `::test_post_redirect_target_is_reachable` is the second
    control (the knob really does redirect to a project that answers); it is cited
    rather than duplicated, because this package adds no fixture knob.

    Mutation: change `argv_with`'s flag to `http.followRedirects=true` — git
    follows, the sibling receives the receive-pack POST, and the sibling-absence
    half reds. **Merely deleting the flag does not**, which was measured rather
    than assumed: git's default is `followRedirects=initial`, and the
    receive-pack POST is not the initial request of the push, so the redirect is
    still refused. Recorded because deleting the flag is the obvious mutation and
    it produces a green.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.git_create_project(SIBLING_PROJECT)
    fake_forge.git_http_redirect_receive_pack = f"{fake_forge.git_url(SIBLING_PROJECT)}/git-receive-pack"

    result = announce_over_git(ocx, fake_forge, package, shim, home)

    assert result.returncode != 0, f"a refused redirect is a failed push: {result.stdout}"
    posts = receive_pack_posts(fake_forge)
    assert len(posts) == 1, f"exactly one receive-pack POST reached the index: {[p.status for p in posts]}"
    assert posts[0].status == 302, f"and it was answered with the redirect: {posts[0].status}"
    authorization = posts[0].header_values("Authorization")
    assert len(authorization) == 1, (
        "the control: the credential was live on the request that was redirected — "
        f"without it the sibling absence below is about nothing: {authorization}"
    )
    sibling = [r for r in fake_forge.git_http_requests if r.project == SIBLING_PROJECT]
    assert sibling == [], f"the redirect target was contacted: {[(r.method, r.path) for r in sibling]}"


def test_header_not_sent_to_a_sibling_project_on_the_same_host(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """S-040/C-034: the `extraHeader` credential ocx produced is **not** sent to a
    second project on the same host, and git's own URL normalisation is what is
    tested.

    Written as a **replay of ocx's own produced configuration**, not as a second
    ocx run: no ocx run ever addresses a sibling project, so S-040 is unreachable
    as an ocx invocation, and a hand-fabricated header would test git rather than
    ocx. The `GIT_CONFIG_COUNT`/`KEY_0`/`VALUE_0` triple is captured from the
    shim's record of the **real** push and replayed through `run_git`, which is
    the module's stated route to "again through git's own URL normalisation".

    Four normalisation variants ride the same replay: the index URL itself, a
    trailing slash, the `.git`-less spelling, and the sibling. The uppercase-host
    variant has no form here — the server binds `127.0.0.1`, an IP literal with
    no uppercase spelling — and that is recorded rather than faked.

    Mutations: widen `CredentialScope::new` to accept a one-segment path and pass
    the host — the sibling replay carries the header and reds; strip `.git` from
    the scope — the index replay stops carrying it and the positive half reds.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path)
    fake_forge.git_create_project(SIBLING_PROJECT)
    fake_forge.git_seed_files(SIBLING_PROJECT, INITIAL_BRANCH, {"README.md": b"a sibling\n"})

    report(announce_over_git(ocx, fake_forge, package, shim, home))

    push = one_invocation(shim, "push")
    injected = {name: push.env[name] for name in INJECTED_ENV}
    assert injected["GIT_CONFIG_COUNT"] == "1", f"the real push must have injected one config: {injected}"

    def replay(url: str) -> tuple[str, ...]:
        before = len(fake_forge.git_http_requests)
        run_git("ls-remote", url, home=home.path, extra_env=injected, check=False)
        made = fake_forge.git_http_requests[before:]
        assert made, f"the replay against {url} reached the server not at all"
        return tuple(value for request in made for value in request.header_values("Authorization"))

    index_url = fake_forge.git_url(INDEX_FULL)
    assert replay(index_url), (
        "the positive half: ocx's own configuration DOES authorize the index project"
    )
    assert replay(index_url + "/"), "a trailing slash normalises to the same prefix"
    assert replay(fake_forge.git_url(INDEX_FULL, dot_git=False)) == (), (
        "the scope carries the `.git` suffix, so the suffix-less spelling is a "
        "different prefix and carries no header"
    )
    assert replay(fake_forge.git_url(SIBLING_PROJECT)) == (), (
        "a second project on the same host must never see the index's credential"
    )


def test_chunked_push_still_delivers_the_four_options(
    ocx: OcxRunner, fake_forge: FakeForge, git_package: tuple[str, str, str], tmp_path: Path
) -> None:
    """C-074: a push forced onto **chunked** framing still delivers the four
    option keys, and the framing is asserted rather than the push's success.

    A claim-sized push is far below `http.postBuffer`'s 1 MiB default, so an
    ordinary push is `Content-Length`-framed and a test named "chunked" passes
    without the chunked path ever running. `BodyFraming` is the only negative
    control that separates the two.

    The POST is selected **by identity**, never by index (DX-19b): a chunked push
    is always **two** receive-pack POSTs, because git sends a `probe_rpc` — a
    4-byte `0000` `Content-Length` body — first, since a chunked stream cannot be
    replayed after a 401. `posts[0]` is that probe.

    `65536` is the only `HOME`-side value that both survives a protocol-v2 clone
    and forces chunked framing (DX-19c): anything at or below `LARGE_PACKET_MAX`
    (65520) aborts every fetch with `BUG: remote-curl.c`. Lowering it to 65520
    kills the *clone*, which is the control that the value matters.
    """
    package, shim, home = prepare(ocx, fake_forge, git_package, tmp_path, post_buffer=65536)

    report(announce_over_git(ocx, fake_forge, package, shim, home))

    posts = receive_pack_posts(fake_forge)
    carrying_a_pack = [post for post in posts if post.body_bytes > 4]
    assert len(carrying_a_pack) == 1, (
        "exactly one POST carries the pack; the others are the probe: "
        f"{[(p.body_bytes, str(p.framing)) for p in posts]}"
    )
    assert carrying_a_pack[0].framing.value == "chunked", (
        "the framing IS the check — a Content-Length body means the chunked "
        f"decoder never ran: {carrying_a_pack[0].framing}"
    )
    keys = sorted(option.split("=", 1)[0] for option in fake_forge.git_pushes(INDEX_FULL)[0].options)
    assert keys == EXPECTED_OPTION_KEYS, f"chunked framing changes nothing about the options: {keys}"


# ══════════════════════════════════════════════════════════════════════════
# S-013 / S-014 — the claim command over the git transport
# ══════════════════════════════════════════════════════════════════════════


def test_first_claim_over_git_opens_a_merge_request(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-013 as the plan names it: a first claim over `--transport git` against a
    GitLab index opens a merge request.

    Until wave 6 this was a strict xfail. `claim/request.rs::request_body`
    renders a multi-line markdown body, `publish_over_git` passes it as the
    `merge_request.description` push option, and `render_push_options` refuses
    every byte outside `0x20..=0x7E` — so every claim over git died at
    `PushOptionRefused`, which `ForgeError::classify` leaves deliberately
    unclassified: exit **1**, `{"kind":"internal"}`, `byte 41 is U+000A`. A
    governance command reporting an internal error for a body it rendered itself.

    The wire clause is the fix's own evidence and is why this row does not stop
    at exit 0: the delivered `.description` must carry **no** raw newline and
    **must** carry the two-character `\n` escape GitLab converts back at parse
    time. A build that made the body single-line instead would pass an exit-code
    assertion while quietly dropping the structure C-067 specifies.

    Read out of the bare repository's own `post-receive` hook, so these are the
    bytes the server received rather than the ones ocx believed it sent.

    Mutations: drop the `escape_newlines` call in `GitWorkspace::push` (the run
    is refused at 1 again); escape into `.title` or `.target` as well, which is
    **not** observable here and deliberately gets no row — this claim's title is
    single-line by construction, so an acceptance assertion on it would be green
    in every state. That mutation's guard is the unit row
    `git_workspace.rs::push_escapes_the_description_and_refuses_a_newline_in_title_or_target`,
    which drives the escape through `push` where it is composed with the
    renderer; the renderer-level
    `git_push_options.rs::the_escape_launders_nothing_but_the_newline` is not
    that guard, because the renderer never sees the escape.
    """
    shim, home = prepare_claim(fake_forge, tmp_path)

    claimed = report(claim_over_git(ocx, fake_forge, shim, home))

    assert claimed["transport"] == "git"
    assert claimed["branch"] == CLAIM_BRANCH
    assert claimed["owners"] == [{"login": CLAIM_OWNER_LOGIN, "id": CLAIM_OWNER_ID}]
    assert claimed["pull_request_number"] is not None, (
        f"the merge request must be confirmed by the poll: {claimed}"
    )

    fetch = one_invocation(shim, "fetch")
    refspecs = [argument for argument in fetch.argv if ":refs/remotes/o/" in argument]
    assert refspecs == [f"{INITIAL_BRANCH}:refs/remotes/o/{INITIAL_BRANCH}"], (
        f"a first claim has no branch to fetch (C-036): {fetch.argv[1:]}"
    )

    pushes = fake_forge.git_pushes(INDEX_FULL)
    assert len(pushes) == 1, f"exactly one push: {pushes}"
    options = dict(option.split("=", 1) for option in pushes[0].options if "=" in option)
    assert sorted(option.split("=", 1)[0] for option in pushes[0].options) == EXPECTED_OPTION_KEYS, (
        f"the option key set is the contract: {pushes[0].options}"
    )
    description = options["merge_request.description"]
    assert "\n" not in description, (
        f"a literal newline never reaches a pkt-line — git refuses it: {description!r}"
    )
    assert "\\n" in description, (
        "the multi-line body must travel as GitLab's two-character escape, not be "
        f"flattened away: {description!r}"
    )
    assert CLAIM_PACKAGE in description and "@" not in description, (
        f"C-067: the body names the package and fires no mention: {description!r}"
    )


def test_claim_inside_a_gitlab_job_authors_with_the_job_token(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-014/C-026/C-045/C-063: the same claim inside a GitLab job with no ocx
    variable — `CI_JOB_TOKEN` is picked up for **both** halves, and the commit
    still authors as ocx.

    The claim twin of `::test_job_token_pickup_headers_and_push_user`, and not a
    duplicate of it: S-014 is worded as a claim, and until wave 6 no claim could
    reach a push at all, so the credential ladder was only ever observed under
    announce. Four clauses from one run, because each is satisfied by a build
    that got the others right — the report's two credential fields, the REST
    reads' `JOB-TOKEN` header, the injected Basic pair decoded back out of the
    recorded child, and the identity read off the pushed commit.

    `admit_publishing_project` is required, not decoration: `CI_PROJECT_PATH` is
    a different project from the index, so the push is cross-project and an
    unseeded allowlist is an EMPTY one, which refuses at 86.

    S-014's remaining clause — "the push authors as the invoking human" — is
    GitLab-side attribution the fixture cannot model; the half ocx controls is
    the commit identity, and `build_fixture_home` writes a deliberately
    different `user.name` so `HOME` cannot be what supplied it.

    Mutation: delete `GIT_AUTHOR_NAME`/`GIT_COMMITTER_NAME` from `SET` — git
    falls back to the fixture `HOME`'s identity and the last clause reds.
    """
    shim, home = prepare_claim(fake_forge, tmp_path)
    admit_publishing_project(fake_forge)

    claimed = report(claim_over_git(ocx, fake_forge, shim, home, token=None, extra_env=job_token_env()))

    assert claimed["credential_kind"] == "job-token", f"the REST half: {claimed}"
    assert claimed["push_credential_kind"] == "job-token", f"the push half: {claimed}"
    headers = [entry for entry in fake_forge.auth_headers if entry]
    assert headers, (
        "the REST fake enforces no authentication — only the git bridge does — so "
        "an empty header log proves nothing"
    )
    assert all("JOB-TOKEN" in entry for entry in headers), (
        f"a job token travels under its own header, never PRIVATE-TOKEN: {headers}"
    )

    user, secret = decoded_injection(one_invocation(shim, "push"))
    assert (user, secret) == (DEFAULT_PUSH_USERNAME, JOB_TOKEN), (
        "the push half must be the job token under GitLab's fixed Basic user"
    )

    identity = run_git(
        "-C", str(fake_forge.git_repository_path(INDEX_FULL)),
        "log", "-1", "--format=%an <%ae>", f"refs/heads/{CLAIM_BRANCH}",
        home=fake_forge.git_scratch_home,
    ).stdout.strip()
    assert identity == "ocx <noreply@ocx.sh>", (
        f"C-045 fixes the commit identity; the fixture HOME's would be "
        f"{home.user_name!r}: {identity!r}"
    )


# ══════════════════════════════════════════════════════════════════════════
# The remote refuses the credential on the fetch
# ══════════════════════════════════════════════════════════════════════════

#: The two ways a git remote refuses the write credential, as
#: `(arm, status)` where `arm(fake_forge)` puts the server in that state.
#:
#: **Both, not one.** `git_stderr.rs`'s `CREDENTIAL_REJECTIONS` carries a
#: different needle per status — git's own `could not read Username for` and
#: libcurl's `The requested URL returned error: 403` — so a build that lost
#: either row keeps classifying the other, and a single-arm row would report that
#: as clean while half the table matched nothing.
#:
#: Both refuse at `GET /info/refs?service=git-upload-pack`, the first request
#: `GitWorkspace::open`'s fetch makes, so nothing is pushed on either arm. That
#: needs a **private** index project (`git_http_private`) and is the rarer half:
#: against a public one the fetch succeeds and the refusal lands on the push
#: instead. Both invocations now read the same table — the scope column is what
#: keeps the 403 row a capability verdict on a push — and the push half is
#: pinned next door by
#: `::test_claim_over_git_exits_80_when_the_push_credential_is_rejected`.
def _private_with_a_different_secret(fake_forge: FakeForge) -> None:
    """A private index project whose accepted secret is not the one ocx resolves.

    Two settings, because they answer different questions: the pair is what makes
    ocx's credential *wrong* rather than merely unknown, and `git_http_private` is
    what puts the refusal on the fetch. Without the second the project is public,
    the fetch succeeds, and the run is refused at the push instead — the same
    table read under the other `GitInvocation`, covered by
    `::test_claim_over_git_exits_80_when_the_push_credential_is_rejected`.
    """
    fake_forge.git_http_private = True
    fake_forge.git_http_credential = (DEFAULT_PUSH_USERNAME, "not-the-secret-ocx-resolves")


FETCH_REFUSALS: Mapping[str, tuple[Any, int]] = {
    "rejected-credential": (
        lambda forge: _private_with_a_different_secret(forge),
        401,
    ),
    "forbidden-project": (lambda forge: setattr(forge, "git_http_forbid_fetch", True), 403),
}


@pytest.mark.parametrize("arm", sorted(FETCH_REFUSALS))
def test_claim_over_git_exits_80_when_the_remote_rejects_the_credential(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path, arm: str
) -> None:
    """The published claim table's "the credential was rejected (401/403)" row,
    end to end: `ocx package claim --transport git` against a remote that refuses
    the fetch exits **80**.

    Until the bridge grew an authorization gate this was unwritable, and the
    defect it hid was live: a 401 or a 403 on the git transport's fetch surfaced
    as an unclassified `GitCommandFailed` and exited **1**, while the table
    promises 80 in every mode but `--out`. A fixture that served every read to
    anyone had no state in which the promise could be tested at all.

    **The `WWW-Authenticate` challenge is NOT what either arm rests on**, and a
    reader would reasonably assume it is. Measured: deleting the challenge from
    `_git_http_authorize` leaves both arms green, because git converts a 401 into
    a credential request with or without one and prints the same `could not read
    Username for` line the classifier matches. The challenge is sent for fidelity
    to GitLab, not as a guard — see `_BASIC_CHALLENGE`.

    The status is asserted through ocx's own message rather than only through the
    exit code, because 80 is shared by both arms and by the preflight refusals:
    a build that classified every git failure as an auth error would pass an
    exit-code-only assertion on both.

    Mutations: return `True` unconditionally from `_git_http_authorize`'s
    non-writing arm (both arms exit 0 and red); delete either row from
    `git_stderr.rs`'s `CREDENTIAL_REJECTIONS` (that arm exits 1 and reds).
    """
    shim, home = prepare_claim(fake_forge, tmp_path)
    refuse, status = FETCH_REFUSALS[arm]
    refuse(fake_forge)

    result = claim_over_git(ocx, fake_forge, shim, home)

    assert result.returncode == 80, (
        f"a remote that rejects the credential is an auth failure, not a generic "
        f"git failure: rc={result.returncode} {result.stderr}"
    )
    assert f"HTTP status {status}" in result.stderr, (
        f"the message must name the status the remote actually answered, or 80 "
        f"is being reported for some other refusal: {result.stderr!r}"
    )

    refused = [request for request in fake_forge.git_http_requests if request.status == status]
    assert refused, (
        f"no request was answered {status}, so the run failed somewhere else "
        f"entirely: {[(r.method, r.path, r.status) for r in fake_forge.git_http_requests]}"
    )
    assert all(
        request.service == "git-upload-pack" and request.path.endswith("/info/refs")
        for request in refused
    ), (
        f"the refusal belongs on the fetch's advertisement, which is where "
        f"`GitWorkspace::open` fails: {[(r.method, r.path) for r in refused]}"
    )

    pushed = [r for r in fake_forge.git_http_requests if r.service == "git-receive-pack"]
    assert pushed == [], (
        f"a run that could not fetch must never have reached a push: "
        f"{[(r.method, r.path, r.status) for r in pushed]}"
    )
    assert fake_forge.git_head(INDEX_FULL, CLAIM_BRANCH) is None, "and must have landed no branch"
    writes = [(method, path) for method, path in fake_forge.requests if method != "GET"]
    assert writes == [], f"S-026: and no REST write stood in for it: {writes}"


def test_claim_over_git_exits_80_when_the_push_credential_is_rejected(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """The same contract on **the path production actually takes**: a credential
    the remote rejects at `git-receive-pack` exits 80, not 1.

    **The index project is public.** GitLab serves a public project's
    `upload-pack` to anyone, so a run whose credential the forge rejects fetches
    successfully, builds its commit, and is refused at the push. The two rows
    above reach the *other* shape, a private project refusing the fetch — the
    rarer one, and for a while the only one classified. This row was a strict
    xfail until `classify_push_failure` started consulting
    `CREDENTIAL_REJECTIONS` before its own refusal walk, which is what makes one
    table serve both invocations.

    Observed before the fix, with the fixture set to accept one secret and ocx
    resolving another: `ERROR git push failed: fatal: could not read Username
    for '<url>': terminal prompts disabled`, exit **1**. The published claim
    table promises 80 for "the credential was rejected (401/403)" and does not
    qualify it by which git invocation met the refusal.

    The three assertions before the exit code are what stop this passing for the
    wrong reason: exit 80 is also what the *fetch* refusal earns, so without
    them a build that made the project private would look identical.

    Mutations, both measured against this row and both restoring the old exit 1
    with `{"kind":"internal"}` verbatim: scope `git_stderr.rs`'s
    `could not read Username for ` row to `RejectionScope::FetchOnly`, or make
    `RejectionScope::covers` answer `false` for `GitInvocation::Push`. Each
    leaves the two fetch arms above green, which is what makes them specific to
    the push path rather than to the table as a whole.

    Deleting the `classify_remote_failure` call from `classify_push_failure`
    looks like the natural third mutation and is not one: it leaves
    `GitInvocation::Push` unconstructed and `-D dead-code` fails the build, so
    nothing reds.
    """
    shim, home = prepare_claim(fake_forge, tmp_path)
    fake_forge.git_http_credential = (DEFAULT_PUSH_USERNAME, "not-the-secret-ocx-resolves")

    result = claim_over_git(ocx, fake_forge, shim, home)

    fetches = [r for r in fake_forge.git_http_requests if r.service == "git-upload-pack"]
    assert fetches and all(r.status == 200 for r in fetches), (
        f"the public read must have succeeded, or this row is the private-project "
        f"case again: {[(r.method, r.path, r.status) for r in fetches]}"
    )
    refused = [r for r in fake_forge.git_http_requests if r.status == 401]
    assert refused and all(r.service == "git-receive-pack" for r in refused), (
        f"the refusal must land on the push: "
        f"{[(r.method, r.path, r.status) for r in fake_forge.git_http_requests]}"
    )
    assert fake_forge.git_head(INDEX_FULL, CLAIM_BRANCH) is None, "and must have landed no branch"

    assert result.returncode == 80, (
        f"the claim table promises 80 for a rejected credential whichever git "
        f"invocation met it: rc={result.returncode} {result.stderr}"
    )
