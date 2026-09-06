# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package claim` acceptance tests — the REST half.

Covers S-001…S-012 and S-036…S-038 of `plan_index_claim_command.md`: a claim
opened over the forge API, the owner ladder and its refusals, `--out`, the
sixteen-key report, the usage refusals, and the argv-boundary git gate. Every
`--transport git` behaviour PAST the version probe is `test_transport_git.py`'s
(WP-17); this module stops at "the run got past the probe".

Four harness facts decide what these rows can assert. A row designed without
them is green for the wrong reason or unreachable:

* **`fake_forge.users` starts empty** while `GET /user` always answers
  `test-forge-bot` / `1001` (DX-74). The token rung therefore resolves a login
  the forge does not know, and *every* claim run without `--owner` exits **79**
  unless `fake_forge.seed_token_identity()` ran first. This is the single most
  likely way this module ships a wrong red, and
  `::test_unseeded_token_identity_exits_79` is the row that pins it rather than
  leaving it a copied incantation.
* **Neither fake enforces authentication** — zero `401` responses in either
  file. An unauthenticated `--out` run is answered normally on every route,
  which is what makes S-011 reachable at all, and it is why
  `::test_no_credential_exits_80` must assert zero forge calls: with the
  refusal deleted the run would *succeed*, not fail.
* **`OCX_DEFAULT_REGISTRY` is the compose registry, not `ocx.sh`**
  (`test/src/runner.py`). C-047's `name` is built from it, so `claim()` below
  pins it to `ocx.sh` for every row and `::test_claim_json_report_key_set`
  runs the same claim a second time under the harness's own registry — a
  single assertion cannot tell "reads the variable" from "hardcodes the host".
* **The child environment is a whitelist** (`runner.py`), so `GITHUB_ACTOR`,
  `GITLAB_USER_LOGIN`, `CI_JOB_TOKEN` and `CI` are structurally absent. A CI
  rung that fires was armed by the test — and `env::is_ci()` is *not* a live
  second guard for the ambient self-update check, which stays silent only
  because `capture_output=True` makes stderr a pipe (DX-62/DX-73).
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

import pytest
from announce_helpers import (
    FIXED_CLOCK,
    INDEX_OWNER,
    INDEX_REPO,
    TOKEN,
    announce,
    forge_args,
)
from fake_forge import FakeForge
from git_shim import GitShimVariant, install_git_shim

from src.runner import OcxRunner

#: The namespace/package every scenario claims. `acme/widget` is the ADR's own
#: worked example, so the branch name, the root path and the request title in
#: these rows read the same as the contract that specifies them.
PACKAGE = "acme/widget"

#: Where the claimed root lands in the index repository (C-047).
ROOT_PATH = f"p/{PACKAGE}.json"

#: C-054's branch, spelled out rather than derived. The contract says the name
#: is asserted DIRECTLY, not inferred from idempotence, so a helper that
#: recomputed it would let the test agree with a second copy of the rule
#: instead of with the rule.
CLAIM_BRANCH = "indexbot-claim-acme-widget"

#: The physical OCI repository the namespace's packages live in. Never read by
#: claim — it is recorded verbatim in the root (C-047) — so it names a registry
#: no test contacts.
PHYSICAL = "oci://ghcr.io/acme/widget"

#: C-047's documented default registry. Pinned explicitly on every run because
#: the harness's own `OCX_DEFAULT_REGISTRY` is the compose registry, so a row
#: that did not override it would either red against an `ocx.sh` literal or
#: silently agree with a hardcoded host.
CANONICAL_REGISTRY = "ocx.sh"

#: The C-050 existing-root read and the request-open route, as `record()` logs
#: them (no query string).
CONTENTS_ROUTE = f"/repos/{INDEX_OWNER}/{INDEX_REPO}/contents/{ROOT_PATH}"
PULLS_ROUTE = f"/repos/{INDEX_OWNER}/{INDEX_REPO}/pulls"

#: The owner every ladder row resolves, and the id the forge holds for it.
#: Distinct from the token identity (`test-forge-bot` / 1001) so a build that
#: confuses `owners` with `author` reds rather than agreeing with itself.
OWNER_LOGIN = "alice"
OWNER_ID = 7

#: C-060's seventeen keys, in declaration order. Asserted as a LIST: a membership
#: check passes with a key missing and a `serde` reorder passes seventeen
#: `.get()`s.
REPORT_KEYS = [
    "package",
    "name",
    "status",
    "forge",
    "transport",
    "credential_kind",
    "push_credential_kind",
    "author",
    "author_identity_source",
    "owners",
    "owner_identity_source",
    "branch",
    "pull_request_url",
    "pull_request_number",
    "fork",
    "written_paths",
    "capability_checks",
]

#: `CapabilityName::ALL`'s declaration order (S-036). The array is stable across
#: runs, so a set or a length assertion is order-blind.
CAPABILITY_NAMES = ["git-version", "push-access", "job-token-push", "job-token-allowlist"]


def claim(
    ocx: OcxRunner,
    fake_forge: FakeForge,
    *args: str,
    package: str = PACKAGE,
    repository: str | None = PHYSICAL,
    token: str | None = TOKEN,
    check: bool = False,
    extra_env: dict[str, str] | None = None,
    forge: str | None = None,
    registry: str | None = CANONICAL_REGISTRY,
    fmt: str | None = "json",
    log_level: str | None = None,
    path: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx package claim`, pointed at `fake_forge`.

    The claim twin of `announce_helpers.announce`, and deliberately NOT a
    parameter on it: half these rows are refusals, so the default here is
    `check=False` (the announce helper defaults to `check=True`), and the
    claim-specific flags — `--repository`, `--owner`, the three `--upstream-*` —
    have no meaning on the announce side. It stays in this module for the same
    reason the hunt gives: `announce_helpers.py` is WP-17's file, and a claim
    helper tidied into it would break the file-set rule for a shared module.

    `token=None` omits `OCX_ANNOUNCE_TOKEN` entirely, which is the exit-80 row
    and the unauthenticated `--out` rows; passing a token that also appears
    under `CI_JOB_TOKEN` in `extra_env` is what makes `api_is_job_token` true
    without dragging the git transport in (hunt E-11).

    Flags precede the positional (C-057), which is why the package is appended
    last rather than interpolated by the caller.
    """
    env: dict[str, str] = {
        "__OCX_TESTING_FORGE_BASE_URL": fake_forge.base_url,
        "__OCX_TESTING_ANNOUNCE_CLOCK": FIXED_CLOCK,
    }
    if registry is not None:
        env["OCX_DEFAULT_REGISTRY"] = registry
    if token is not None:
        env["OCX_ANNOUNCE_TOKEN"] = token
    if path is not None:
        env["PATH"] = path
    if extra_env:
        env.update(extra_env)
    argv = ["package", "claim", *forge_args(forge)]
    if repository is not None:
        argv += ["--repository", repository]
    argv += [*args, package]
    return ocx.run(*argv, format=fmt, check=check, log_level=log_level, env_overrides=env)


def claim_json(ocx: OcxRunner, fake_forge: FakeForge, *args: str, **kwargs: Any) -> dict:
    """`claim`, asserting the run succeeded, and parsing the report."""
    result = claim(ocx, fake_forge, *args, **kwargs)
    assert result.returncode == 0, f"claim failed (rc={result.returncode}): {result.stderr}"
    return json.loads(result.stdout)


def seed_index_base(
    fake_forge: FakeForge, *, owner: str = INDEX_OWNER, repo: str = INDEX_REPO
) -> None:
    """Give the index repository a `main` that does NOT carry `p/<pkg>.json`.

    The starting state of every claim: the index exists and has a base commit
    to parent on, and the namespace is unclaimed. Seeding an unrelated path is
    enough — the point is a `main` that exists, so the C-050 read answers "no
    root" rather than "no branch", which are different failures.

    Distinct from `announce_helpers.seed_empty_root`, which seeds the root and
    is therefore the ALREADY-claimed state — the one claim refuses at 65.
    """
    fake_forge.seed_files(owner, repo, {"README.md": b"the index\n"})


def seed_claimed_root(fake_forge: FakeForge) -> None:
    """The already-claimed state: a committed root on the index's `main`."""
    fake_forge.seed_root(
        INDEX_OWNER,
        INDEX_REPO,
        ROOT_PATH,
        {"name": f"{CANONICAL_REGISTRY}/{PACKAGE}", "repository": PHYSICAL, "tags": {}},
    )


def claimed_root_bytes(fake_forge: FakeForge) -> bytes:
    """The bytes claim committed onto its own branch."""
    raw = fake_forge.read_file(INDEX_OWNER, INDEX_REPO, ROOT_PATH, branch=CLAIM_BRANCH)
    assert raw is not None, f"no root committed on {INDEX_OWNER}/{INDEX_REPO}:{CLAIM_BRANCH}"
    return raw


def expected_root_bytes(
    *,
    owners: list[dict[str, Any]],
    registry: str = CANONICAL_REGISTRY,
    upstream: dict[str, Any] | None = None,
) -> bytes:
    """C-047's root, built in the contracted FIELD ORDER.

    Python dicts keep insertion order and `json.dumps` does not sort, so the
    literal this returns is order-bearing — which is the whole point: comparing
    two PARSED values is order-independent (`claim/root.rs` states it of
    `IndexMap::eq`), so only a byte compare can see a reordered renderer.

    `upstream` is omitted entirely when absent, never emitted as `null`, and it
    sits between `desc` and `tags` — so the argument order here is the wire
    order and a renderer that appended it after `tags` reds.
    """
    root: dict[str, Any] = {
        "name": f"{registry}/{PACKAGE}",
        "repository": PHYSICAL,
        "owners": owners,
        "status": "active",
        "deprecated_message": None,
        # `created` is a DATE, not a timestamp (C-047), taken from the shared
        # test clock so this literal cannot rot within a day.
        "created": FIXED_CLOCK[:10],
        "desc": None,
    }
    if upstream is not None:
        root["upstream"] = upstream
    root["tags"] = {}
    return (json.dumps(root, indent=2) + "\n").encode()


def write_routes(fake_forge: FakeForge) -> list[tuple[str, str]]:
    """Every recorded request that was not a read.

    `--out` must open nothing, and "no pull request was opened" alone does not
    say that: a run that PATCHed a ref or POSTed a commit wrote to the forge
    just as much.
    """
    return [(method, path) for method, path in fake_forge.requests if method != "GET"]


# ── the claim, the root and the report ────────────────────────────────────


def test_claim_writes_login_id_only(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """S-001/C-047: the committed root's `owners` are `login` + `id` and nothing
    else.

    The owner entry is a governance record, so the wire form is the contract
    (C-008): a fixture-shaped four-key entry, or an entry carrying the forge's
    `bot`/`type` projection, is a different document for every downstream
    reader of the index.

    Asserted as EQUALITY of the whole list, never key membership: an entry that
    also carries `bot` satisfies `entry["login"] == "alice"`.

    Mutation: emit the four-key owner form in `claim/root.rs::render_root`.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    claim_json(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}")

    root = json.loads(claimed_root_bytes(fake_forge))
    assert root["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}], (
        "the governance record must carry exactly login and id"
    )


def test_claim_field_order_byte_exact(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """C-047: the root's fields land in the contracted ORDER, compared as bytes.

    A parsed-value compare cannot see this — `IndexMap::eq` is
    order-independent, which `claim/root.rs` states about itself — so the
    assertion is over the committed bytes against a literal, with
    `__OCX_TESTING_ANNOUNCE_CLOCK` pinned: `created` is a date from the shared
    clock, and an unpinned literal rots within a day.

    `upstream` is OMITTED entirely when absent, never `null`.

    Mutations, each reding this row alone: reorder any two `root.insert` calls;
    emit `null` for an absent `upstream`; emit the four-key owner form.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    claim_json(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}")

    assert claimed_root_bytes(fake_forge) == expected_root_bytes(
        owners=[{"login": OWNER_LOGIN, "id": OWNER_ID}]
    )


def test_claim_branch_name(ocx: OcxRunner, fake_forge: FakeForge) -> None:
    """C-054: the branch is `indexbot-claim-acme-widget`, asserted directly.

    Distinct from the announce branch (`indexbot-announce-...`) so indexbot's
    G-04 `new-package` classification cannot confuse a claim with a refresh.
    The contract requires the name be asserted rather than inferred from
    idempotence — two runs converging on one branch proves determinism, not
    which name.

    Both surfaces from one run: the report's `branch` and the ref the forge
    actually holds. A report that named the branch while the commit landed
    somewhere else would pass either half alone.

    Mutation: rename the prefix in `claim/request.rs`; only a direct assertion
    reds.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    report = claim_json(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}")

    assert report["branch"] == CLAIM_BRANCH
    assert fake_forge.branch_head(INDEX_OWNER, INDEX_REPO, CLAIM_BRANCH) is not None, (
        f"no ref named {CLAIM_BRANCH} exists on the index repository"
    )


def test_claim_json_report_key_set(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-060/S-002: the assembled `--format json` report, key set AND order AND
    value vocabularies.

    No unit test covers this. `ClaimReport::from_outcome` is exercised at unit
    scope, so the MAPPING is covered; what is not covered anywhere is the
    document a real run produces — that serde emits the sixteen names in
    declaration order over an outcome the library actually built, that
    `report_roots!` publishes it, and that no `--format json` framing renames
    anything. `package`, `name`, `branch`, `pull_request_url`,
    `pull_request_number` and `written_paths` are uncovered until this row
    exists.

    `list(report)`, never sixteen `.get()`s: a missing key passes the latter,
    and a serde reorder passes a membership check. Then each vocabulary against
    the run's KNOWN state, and `capability_checks` as an ordered sequence
    (S-036: the array is stable across runs, so a set or a length assertion is
    order-blind).

    `name` is asserted TWICE, once under `OCX_DEFAULT_REGISTRY=ocx.sh` and once
    under the harness's own registry: a single assertion cannot tell "reads the
    variable" from "hardcodes `ocx.sh`", and the harness default is not
    `ocx.sh`. The second run is `--out` so it needs no second request.

    Mutations: move any field in `ClaimReport`; drop the
    `match transport { Api => None, .. }` gate in `forge_report`'s
    `push_credential_kind` (it becomes `"token"` on an ordinary REST claim);
    sort `capability_checks`; hardcode `ocx.sh` in `root_name`.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    report = claim_json(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}")

    assert list(report) == REPORT_KEYS, "the sixteen keys, in declaration order"
    assert report["package"] == PACKAGE
    assert report["name"] == f"{CANONICAL_REGISTRY}/{PACKAGE}"
    assert report["status"] == "updated"
    assert report["forge"] == "github"
    assert report["transport"] == "api"
    assert report["credential_kind"] == "token"
    assert report["push_credential_kind"] is None, (
        "`push_credential_kind` is null under the api transport (C-060); "
        "`ForgeCredentials::resolve` populates the push half whenever an API "
        "credential exists, so a report rendered from it alone says 'token'"
    )
    assert report["author"] == {"login": "test-forge-bot", "id": 1001}
    assert report["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}]
    assert report["owner_identity_source"] == "resolved"
    assert report["branch"] == CLAIM_BRANCH
    assert report["pull_request_url"] == (
        f"{fake_forge.base_url}/{INDEX_OWNER}/{INDEX_REPO}/pull/1"
    )
    assert report["pull_request_number"] == 1
    assert report["fork"] is None
    assert report["written_paths"] == []
    assert [check["name"] for check in report["capability_checks"]] == CAPABILITY_NAMES
    assert report["capability_checks"] == [
        {"name": "git-version", "status": "skipped", "detail": None},
        {"name": "push-access", "status": "passed", "detail": None},
        {"name": "job-token-push", "status": "skipped", "detail": None},
        {"name": "job-token-allowlist", "status": "skipped", "detail": None},
    ]

    under_harness_registry = claim_json(
        ocx,
        fake_forge,
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--out",
        str(tmp_path / "out"),
        registry=ocx.registry,
    )
    assert under_harness_registry["name"] == f"{ocx.registry}/{PACKAGE}", (
        "`name` must be built from the resolved default registry, not from a "
        "hardcoded `ocx.sh`"
    )


def test_claim_report_plain_is_five_columns(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """C-060 (hunt E-23): the plain render is five columns, at acceptance scope.

    WP-14's unit seam covers the table's CONSTRUCTION; nothing covers that
    `print_table` emits it to stdout under the plain format, which is the only
    surface a human sees. A `Printable` wiring mistake shows up here and
    nowhere else.

    The header names are asserted in ORDER, over one line, so a swapped pair
    reds; and the row is asserted to carry the branch, so a header-only render
    (five columns of nothing) does not pass.

    Mutation: register the report with no `Printable` impl, or swap two
    `Column`s.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    result = claim(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}", fmt=None)

    assert result.returncode == 0, result.stderr
    header = next(
        (line for line in result.stdout.splitlines() if "Package" in line), ""
    )
    positions = [header.find(name) for name in ("Package", "Status", "Transport", "Branch", "Pull Request")]
    assert all(position >= 0 for position in positions), (
        f"the five column headers are not all present: {header!r}"
    )
    assert positions == sorted(positions), (
        f"the five columns are not in the contracted order: {header!r}"
    )
    assert CLAIM_BRANCH in result.stdout, (
        "the Branch column must carry a value, not only a header"
    )


def test_rerun_updates_same_request(ocx: OcxRunner, fake_forge: FakeForge) -> None:
    """S-003: re-running before the request merges updates it, never duplicates
    it. TWO reruns, because the scenario has two halves a single rerun cannot
    show.

    The IDENTICAL rerun is C-051's `Ahead` byte-identical case: no write at
    all, `status: "unchanged"`, the same `pull_request_number`, and the recorded
    POST count for the pull-request route unchanged. The rerun with a CHANGED
    `--owner` set reports `status: "updated"` and the SAME number — the request
    is updated, not reopened.

    Mutations: compare parsed values instead of bytes on the `Ahead` path — the
    identical rerun reports `updated`; open a second request instead of reusing
    the open one — the number moves on the second half.

    **S-003's error case — `Ahead` with a moved base, then 75 — is NOT here,
    and is not reachable at this scope.** 75 is a PERSISTENT non-fast-forward:
    `claim::claim` re-reads and retries exactly once, so the ref update must be
    rejected TWICE, while `fake_forge.concurrent_ref_advance` pops its key when
    it fires and the retry therefore always succeeds. Reaching it would need a
    new persistently-rejecting knob on the shared ref-update handler — the one
    path every announce module writes through. The single retry is unit-covered
    (`claim.rs::non_fast_forward_retry_regenerates_exactly_once`) and the git
    twin is `test_transport_git.py`'s, so this is recorded as an open cell
    rather than paid for with a fixture change wave 5 cannot afford.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)
    fake_forge.seed_user("bob", 8)

    first = claim_json(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}")
    assert first["status"] == "updated"
    opens_after_first = fake_forge.request_count("POST", PULLS_ROUTE)
    assert opens_after_first == 1, (
        "the counter both reruns are compared against is pinned NON-ZERO here: "
        f"if `{PULLS_ROUTE}` ever stopped matching what `record()` logs, both "
        "sides of that comparison would be 0 and it would pass vacuously"
    )

    identical = claim_json(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}")
    assert identical["status"] == "unchanged", (
        "a byte-identical rerun writes nothing (C-051 `Ahead` byte-identical)"
    )
    assert identical["pull_request_number"] == first["pull_request_number"]
    assert fake_forge.request_count("POST", PULLS_ROUTE) == opens_after_first, (
        "the byte-identical rerun must not open a second request"
    )

    moved = claim_json(
        ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}", "--owner", "bob:8"
    )
    assert moved["status"] == "updated"
    assert moved["pull_request_number"] == first["pull_request_number"], (
        "the open request is UPDATED, not reopened"
    )


def test_forge_5xx_on_the_request_open_exits_69(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """S-001's third error case: a forge 5xx is exit **69**.

    S-001's error column is `no credential -> 80; committed root exists -> 65;
    forge 5xx -> 69`. The first two have rows (`::test_no_credential_exits_80`,
    `::test_existing_root_refused_65`); without this one the headline scenario
    ships with a third of its contracted error surface untested end to end.

    69 (`EX_UNAVAILABLE`) is the signal a CI wrapper retries on, and it is the
    one claim exit code that says "nothing about your invocation was wrong".
    Reaching it needs no new fixture knob: `pull_fail_once` makes the NEXT
    `POST .../pulls` reply 500 once, the failure lands AFTER the commit (so the
    5xx is genuinely mid-run, not a refusal), and claim's only retry is the
    `NonFastForward` one — a 500 on the request open propagates.

    The cleared knob is the positive control: it re-arms nothing and clears
    itself when the arm fires, so `pull_fail_once is False` afterwards is the
    proof the 500 was actually served rather than the run failing at 69 for an
    unrelated reason. The POST count pins that the open was attempted exactly
    once — a silent retry loop would report a different number.

    Mutation: map `ForgeError::Status` in `(500..=599)` to any other exit code
    in `forge/error.rs`'s table — this row reds and no other row in the module
    moves.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)
    fake_forge.pull_fail_once = True

    result = claim(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}")

    assert result.returncode == 69, result.stderr
    assert fake_forge.pull_fail_once is False, (
        "the 500 arm never fired, so 69 came from somewhere other than the "
        "forge-side failure this row is about"
    )
    assert fake_forge.request_count("POST", PULLS_ROUTE) == 1, (
        "the request open is attempted exactly once; claim retries only on a "
        f"non-fast-forward: {fake_forge.requests}"
    )


@pytest.mark.parametrize("mode", ["direct", "out", "fork"])
def test_existing_root_refused_65(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path, mode: str
) -> None:
    """S-004/C-050: a root already committed at `p/<ns>/<pkg>.json` is refused
    at **65**, with a message pointing at `ocx package announce`.

    C-050 holds in EVERY mode, and the plan names only the bare and the `--out`
    variants — so this row is parametrised over all three targets, the `--fork`
    arm reaching the refusal through a different early return.

    S-037 depends on the code being exactly 65 and on the message naming the
    other command: a release wrapper branches on the pair 65-from-claim /
    79-from-announce, so an exit-code-only assertion leaves the half that tells
    an operator what to do next untested.

    Mutation: guard the C-050 read on the target — guarding it against `Out`
    reds one arm, against `Direct` reds two.
    """
    seed_claimed_root(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)
    target = {
        "direct": [],
        "out": ["--out", str(tmp_path / "out")],
        "fork": ["--fork", f"forkuser/{INDEX_REPO}"],
    }[mode]

    result = claim(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}", *target)

    assert result.returncode == 65, result.stderr
    assert "ocx package announce" in result.stderr, (
        f"the refusal must point at the other command: {result.stderr!r}"
    )


def test_disclaimer_reaches_root_not_request_body(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """S-038/C-067: an `--upstream-disclaimer` carrying markdown and an
    `@mention` reaches the ROOT FILE only.

    Both polarities in one run, because each half alone is vacuous: the request
    TITLE and BODY contain neither the disclaimer text, nor an `@`, nor the
    markdown link (a body-only assertion leaves the title open), and the root
    DOES carry it (the positive control — without it, "the disclaimer never
    reached anywhere" passes).

    The same row asserts owners render as bare `login:id` in the body and never
    as `@login`. A repository whose reviewers are mass-mentioned by an automated
    claim is the failure C-067 exists to prevent.

    Mutations: interpolate the disclaimer into `claim/request.rs::request_body`
    — the absence reds; drop it from `render_root` — the presence reds; prefix
    the owner pairs with `@` — the mention assertion reds.
    """
    disclaimer = "Repackaged by [the Acme team](https://acme.invalid/team) — ping @acme-team"
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    claim_json(
        ocx,
        fake_forge,
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--upstream-org",
        "Acme Org",
        "--upstream-disclaimer",
        disclaimer,
    )

    root = json.loads(claimed_root_bytes(fake_forge))
    assert root["upstream"]["disclaimer"] == disclaimer, (
        "the positive control: the disclaimer must reach the root file"
    )

    opened = [body for path, body in fake_forge.bodies if path == PULLS_ROUTE]
    assert len(opened) == 1, f"expected exactly one opened request, got {len(opened)}"
    title = opened[0].get("title", "")
    body = opened[0].get("body", "")
    for field, text in (("title", title), ("body", body)):
        assert disclaimer not in text, f"the disclaimer reached the request {field}"
        assert "@" not in text, f"the request {field} carries an at-sign: {text!r}"
        assert "](" not in text, f"the request {field} carries a markdown link: {text!r}"
    assert f"{OWNER_LOGIN}:{OWNER_ID}" in body, (
        "owners render as bare `login:id` in the request body (C-067)"
    )


# ── --out mode, and the refusals that must still hold under it ────────────


def test_out_renders_and_opens_nothing(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-010: `--out` writes the root under the directory and opens no request.

    "Opens nothing" as an absence over an unconstrained request log is vacuous
    in BOTH directions — a run that never contacted the forge passes it, and so
    does one that opened a request against a repository the assertion does not
    name. So this row asserts the positive half too: the C-050 root read and
    the owner lookup DID happen, and no write route was called.

    Plus `written_paths == ["p/acme/widget.json"]` (one relative path),
    `pull_request_url is None`, `pull_request_number is None`, and
    `branch == CLAIM_BRANCH` by VALUE — the branch is always populated, so
    "not null" would pass in every state.

    The run carries a credential and still reports `credential_kind: "token"`
    (hunt E-18). This is the ONLY row in the module that occupies that cell:
    the other `credential_kind` assertions are the direct-transport run
    (`::test_claim_json_report_key_set`) and the credential-LESS `--out` run
    (`::test_out_without_credential_reports_push_access_skipped`, which owes
    `"none"`). `--out` returns early inside `claim::claim`, which is exactly
    where a "treat `--out` as unauthenticated" shortcut gets written — and it
    would report `none` for a run that DID authenticate both of its reads.

    Mutations: return before the C-050 read on the `--out` path — the positive
    half reds; fall through to the pull-request open — the negative half reds;
    blank the credential kind whenever `written_paths` is non-empty — only the
    `credential_kind` line reds.
    """
    out_dir = tmp_path / "out"
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    report = claim_json(
        ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}", "--out", str(out_dir)
    )

    assert report["written_paths"] == [ROOT_PATH]
    assert (out_dir / ROOT_PATH).read_bytes() == expected_root_bytes(
        owners=[{"login": OWNER_LOGIN, "id": OWNER_ID}]
    )
    assert report["pull_request_url"] is None
    assert report["pull_request_number"] is None
    assert report["branch"] == CLAIM_BRANCH
    assert report["status"] == "updated", "a claim `--out` run always reports updated (C-060)"
    assert report["credential_kind"] == "token", (
        "`--out` with a credential present still reports the credential it "
        "authenticated its reads with (hunt E-18); reporting `none` here would "
        "describe a run that did authenticate as unauthenticated"
    )

    assert fake_forge.request_count("GET", CONTENTS_ROUTE) >= 1, (
        "the C-050 existing-root read still happens under `--out` (S-010)"
    )
    assert fake_forge.request_count("GET", f"/users/{OWNER_LOGIN}") >= 1, (
        "the owner ladder still resolves against the forge under `--out`"
    )
    assert write_routes(fake_forge) == [], (
        f"`--out` opened a write route: {write_routes(fake_forge)}"
    )


def test_out_without_credential_reports_push_access_skipped(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-011/S-036/C-069: `--out` with no credential proceeds unauthenticated
    and still reports a NON-EMPTY `capability_checks`.

    All four rows, as an ordered sequence, each `skipped` — not a lone
    `push-access: skipped` lookup, which passes for a renderer that filters
    `skipped` rows out and leaves the array empty. That "omit what does not
    apply" instinct is exactly what C-069 exists against.

    Reachable because the fake enforces no authentication; it needs no
    `--owner`, only `seed_token_identity()` (DX-74).

    Mutations: filter `status == "skipped"` in the capability-row renderer —
    the array becomes `[]`; call `ensure_push_access` on the `--out` path —
    GitHub answers `passed` and the row reds.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_token_identity()

    report = claim_json(
        ocx, fake_forge, "--out", str(tmp_path / "out"), token=None
    )

    assert report["credential_kind"] == "none"
    assert report["capability_checks"], "capability_checks is non-empty on EVERY run (C-069)"
    assert [check["name"] for check in report["capability_checks"]] == CAPABILITY_NAMES
    assert [check["status"] for check in report["capability_checks"]] == ["skipped"] * 4


def test_out_still_refuses_existing_root(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-050 under `--out`: exit **65**, message naming `ocx package announce`,
    and NOTHING written under the output directory.

    The plan names the refusal; nothing names the tree. A build that writes
    first and refuses second exits 65 too, and leaves a half-materialised
    directory the operator then commits.

    Mutation: move the C-050 read below the write — the exit code is unchanged
    and only the directory assertion reds.
    """
    out_dir = tmp_path / "out"
    seed_claimed_root(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    result = claim(
        ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}", "--out", str(out_dir)
    )

    assert result.returncode == 65, result.stderr
    assert "ocx package announce" in result.stderr
    written = sorted(str(p.relative_to(out_dir)) for p in out_dir.rglob("*")) if out_dir.exists() else []
    assert written == [], f"the refused run left a half-materialised tree: {written}"


def test_out_writes_under_a_nested_directory(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-010 (hunt E-19): `--out` into a path whose parents do not exist.

    `write_out` creates parents; neither the creation nor its failure has any
    named test in the plan. The nested target is two levels below anything that
    exists, so a writer using `create_dir` rather than `create_dir_all` reds.

    Mutation: replace `create_dir_all` with `create_dir`.
    """
    out_dir = tmp_path / "absent" / "parents" / "out"
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    report = claim_json(
        ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}", "--out", str(out_dir)
    )

    assert report["written_paths"] == [ROOT_PATH]
    assert (out_dir / ROOT_PATH).is_file(), (
        f"the nested output path was not created: {out_dir}"
    )


def test_out_write_failure_exits_74(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-010 (hunt E-19): a failed `--out` write is exit **74**.

    74 is the only claim-owned exit code with no row anywhere in the plan, so
    the path ships unexercised without this one. The failure is produced by a
    parent that exists and is a REGULAR FILE — `create_dir_all` cannot make a
    directory under it, on any platform, and no permission bit or root-user
    quirk is involved.

    Asserted as the code alone, deliberately: `OutputWrite` wraps the OS error,
    whose text is the platform's, so a message assertion here would pin the
    libc string rather than the contract.

    Mutation: map the write failure to any other `ClaimError` variant — the
    exit code moves off 74.
    """
    blocker = tmp_path / "blocker"
    blocker.write_text("not a directory\n")
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    result = claim(
        ocx,
        fake_forge,
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--out",
        str(blocker / "out"),
    )

    assert result.returncode == 74, result.stderr


# ── the owner ladder ──────────────────────────────────────────────────────


def test_owner_explicit_replaces(ocx: OcxRunner, fake_forge: FakeForge) -> None:
    """S-006/C-048: `--owner alice --owner bob` yields exactly those two, in
    ORDER, and the invoker is NOT added.

    `test-forge-bot` is seeded as well as `alice` and `bob`. "The invoker is not
    added" is only falsifiable when adding it would SUCCEED: with the identity
    unseeded a build that appends it exits 79, so the row reds on the wrong
    contract and sends its reader to the wrong line.

    Order, not a set — `--owner` order is the root's order, which is what a
    governance reviewer reads. The reversed pair is claimed under a SECOND
    package so the two runs cannot converge on one branch and hide a sort.

    **The confirmed half of the duplicate rule lives here** (DX-79). As first
    shipped, `--owner alice --owner alice` exited **0** on this path recording
    ONE owner while the same argv with the users API out of reach was refused
    at 64 — one argv, two outcomes, decided by an external service, over a list
    a human merges under G-04. `resolve_owners` now refuses a verbatim repeat
    over the SUPPLIED spellings before either path can confirm them, so this
    row and the unconfirmed one in
    `::test_owner_asserted_needs_a_gitlab_job_token` are the two halves of one
    rule and neither alone can see the fix.

    The refusal is deliberately not case-folding: two spellings differing only
    in case are still collapsed by the server's canonical login on this path,
    which is C-048 working as specified. That residual is recorded at the site
    in `claim/owners.rs` and pinned by
    `duplicate_owners_are_refused_on_both_paths`, whose third row holds the
    collapse against the two refusals in one function — the shape a subprocess
    row cannot have.

    Mutations: append the detected identity in `seed_logins` — a third entry
    appears; sort the list — the order assertion reds; delete the
    verbatim-repeat refusal from `resolve_owners`' supplied-list loop — the
    duplicate row exits 0 recording one owner.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_token_identity()
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)
    fake_forge.seed_user("bob", 8)

    report = claim_json(ocx, fake_forge, "--owner", OWNER_LOGIN, "--owner", "bob")
    assert report["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}, {"login": "bob", "id": 8}]

    reversed_report = claim_json(
        ocx, fake_forge, "--owner", "bob", "--owner", OWNER_LOGIN, package="acme/gadget"
    )
    assert reversed_report["owners"] == [
        {"login": "bob", "id": 8},
        {"login": OWNER_LOGIN, "id": OWNER_ID},
    ], "`--owner` order is the root's order, so a sorted renderer reds here"

    # A third namespace, so the refusal cannot be confused with anything the
    # two runs above left on their branches.
    duplicated = claim(
        ocx, fake_forge, "--owner", OWNER_LOGIN, "--owner", OWNER_LOGIN, package="acme/gizmo"
    )
    assert duplicated.returncode == 64, duplicated.stderr
    assert OWNER_LOGIN in duplicated.stderr, (
        "a repeated login is refused with the users API REACHABLE too, naming the "
        f"login, instead of silently recording one owner: {duplicated.stderr!r}"
    )



def test_owner_canonical_login_overrides_supplied_case(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """S-007 end to end: `--owner AliCe` against a forge that knows `alice`
    yields `alice`, source `resolved`.

    Asserted in BOTH renderings from one run — the committed root and the JSON
    report. They are two renderings of one `Vec<ResolvedOwner>`, so asserting
    only the report cannot see a root renderer that re-reads the operator's
    spelling, and asserting both keeps the property covered if the two
    renderers ever split.

    Mutation: keep the supplied spelling in `confirm_with_forge` — both
    assertions red.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    report = claim_json(ocx, fake_forge, "--owner", "AliCe")

    assert report["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}]
    assert report["owner_identity_source"] == "resolved"
    root = json.loads(claimed_root_bytes(fake_forge))
    assert root["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}], (
        "the root renderer must carry the forge's spelling, not the operator's"
    )


@pytest.mark.parametrize("half", ["resolved", "ci-environment"])
def test_owner_ci_environment(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path, half: str
) -> None:
    """C-048's CI clause, which has TWO outcomes and therefore two rows.

    (a) S-007: a CI pair with a REACHABLE users API — the API confirms and
    overrides, so the source is **`resolved`**, not `ci-environment`. The
    `GITHUB_ACTOR` spelling is deliberately mixed-case, so the row also shows
    the confirm step overriding what CI supplied.
    (b) S-008: a CI pair on GitLab with the users API out of reach under a job
    token — the list is carried unconfirmed and the source is
    **`ci-environment`**, said identically by the report and the stderr line.

    They differ in which branch of `resolve_owners` answers and produce
    DIFFERENT wire words, which is the one thing S-008 exists to pin; a single
    scenario cannot show both. The GitLab half needs `api_is_job_token`, which
    is reachable without the git transport by exporting the same value under
    `OCX_ANNOUNCE_TOKEN` and `CI_JOB_TOKEN` (hunt E-11).

    Both halves run `--out`, which still performs the whole owner ladder
    (S-010) while keeping the row REST-only. So two of S-008's three surfaces
    are asserted here and the third — the request body — is not, for the plain
    reason that an `--out` run opens no request at all. The body's agreement
    with the outcome is unit-covered by `claim.rs::
    owner_identity_source_is_rendered_once_for_body_and_outcome`, which holds
    the stronger property (ONE rendering of one value, so no second spelling
    can drift) in the one scope where both surfaces are in reach. It is NOT
    `test_transport_git.py`'s: on the default `api` transport the body is a
    REST pull-request body, which this module reads in
    `::test_disclaimer_reaches_root_not_request_body`.

    Mutations: return `CiEnvironment` unconditionally for the CI rung — (a)
    reds; return `Resolved` from the unconfirmed arm — (b) reds.
    """
    seed_index_base(fake_forge)
    out = ["--out", str(tmp_path / "out")]
    if half == "resolved":
        fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)
        report = claim_json(
            ocx,
            fake_forge,
            *out,
            extra_env={"GITHUB_ACTOR": "AliCe", "GITHUB_ACTOR_ID": str(OWNER_ID)},
        )
        assert report["owner_identity_source"] == "resolved"
        assert report["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}]
        return

    fake_forge.users_api_status = 403
    result = claim(
        ocx,
        fake_forge,
        *out,
        forge="gitlab",
        extra_env={
            "GITLAB_USER_LOGIN": OWNER_LOGIN,
            "GITLAB_USER_ID": str(OWNER_ID),
            "CI_JOB_TOKEN": TOKEN,
        },
    )
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report["owner_identity_source"] == "ci-environment"
    assert report["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}]
    assert "ci-environment" in result.stderr, (
        "C-072: the stderr line names the same source word as the report"
    )


def test_owner_asserted_needs_a_gitlab_job_token(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-048 (hunt E-10/DX-72): `owner_identity_source: "asserted"`.

    The third vocabulary word, and the only one the plan never gives a row.
    It is **unreachable on GitHub**: `asserted` needs `confirm_with_forge` to
    see `Err(UsersApiUnavailable)`, GitHub's `resolve_user` maps 404 to
    `Ok(None)` and every other non-success to `Err(Status)`, and only GitLab
    produces `UsersApiUnavailable` — only under `api_is_job_token`. So this is a
    GitLab row by construction, not by preference.

    Its sibling is in the same fixture state and is what makes the word
    discriminating: a BARE `--owner alice` with the users API out of reach is
    refused at 64 naming the `LOGIN:ID` form (S-008's error case). A build that
    took the operator's word for an unconfirmable bare login would pass the
    `asserted` half and red here.

    The third scenario is the UNCONFIRMED half of the duplicate rule: a
    repeated login is refused at 64 here, as it now is on the confirmed path
    (`::test_owner_explicit_replaces` holds that half). Both rows are needed
    and neither is redundant — before DX-79 this state refused while a
    reachable users API exited 0 recording one owner, so this row alone agrees
    with the defect.

    Mutation: return `Resolved` from the unconfirmed arm of `resolve_owners` —
    the `asserted` half reds. (Widening `users_api_is_out_of_reach` to drop the
    job-token conjunct does NOT red either half, which is why that mutation is
    named here and not used.) Deduplicate `--owner` at the CLI — the duplicate
    scenario reds. Deleting the supplied-list refusal does NOT red this half:
    `take_operator_word` still catches it with no id to collapse by, which is
    exactly why the confirmed row exists.
    """
    seed_index_base(fake_forge)
    fake_forge.users_api_status = 403
    job_token_env = {"CI_JOB_TOKEN": TOKEN}

    report = claim_json(
        ocx,
        fake_forge,
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--out",
        str(tmp_path / "asserted"),
        forge="gitlab",
        extra_env=job_token_env,
    )
    assert report["owner_identity_source"] == "asserted"
    assert report["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}]

    bare = claim(
        ocx,
        fake_forge,
        "--owner",
        OWNER_LOGIN,
        "--out",
        str(tmp_path / "bare"),
        forge="gitlab",
        extra_env=job_token_env,
    )
    assert bare.returncode == 64, bare.stderr
    assert "LOGIN:ID" in bare.stderr, (
        f"the refusal must name the form that would work: {bare.stderr!r}"
    )

    duplicated = claim(
        ocx,
        fake_forge,
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--out",
        str(tmp_path / "duplicated"),
        forge="gitlab",
        extra_env=job_token_env,
    )
    assert duplicated.returncode == 64, duplicated.stderr
    assert OWNER_LOGIN in duplicated.stderr, (
        "a repeated login is preserved unmerged at the CLI and refused by the "
        f"library, naming the login: {duplicated.stderr!r}"
    )


def _arrange_weak_login_shape(fake_forge: FakeForge) -> list[str]:
    """(a) the documented bot login shape, refused before any request."""
    return ["--owner", "dependabot[bot]"]


def _arrange_confirmed_bot(fake_forge: FakeForge) -> list[str]:
    """(b) the forge's own `bot` field, on a login no shape matches."""
    fake_forge.seed_user("robotics", 55, bot=True)
    return ["--owner", "robotics"]


def _arrange_detected_bot(fake_forge: FakeForge) -> list[str]:
    """(c) the detected identity is a bot — no `--owner` at all.

    The seeding is deliberately NOT `bot=True`. Seeding the lookup side as a
    bot as well would arm row (b)'s guard on this run too, and deleting the
    guard this row is named for would then leave `confirm_with_forge` refusing
    the same account at the same exit code — a green with its own mutation
    applied. With a HUMAN account behind the login, deleting `seed_logins`'
    `identity.bot` check lets the run confirm it and exit 0, which is the red
    this row owes.
    """
    fake_forge.token_identity_is_bot = True
    fake_forge.seed_token_identity()
    return []


@pytest.mark.parametrize(
    ("row", "arrange"),
    [
        ("weak-login-shape", _arrange_weak_login_shape),
        ("confirmed-bot", _arrange_confirmed_bot),
        ("detected-bot", _arrange_detected_bot),
    ],
)
def test_owner_bot_refused_64(
    ocx: OcxRunner, fake_forge: FakeForge, row: str, arrange
) -> None:
    """S-009/C-049: a bot is refused at **64** naming `--owner`. THREE rows,
    because the contract has three guards at three different lines and one row
    cannot red three.

    Row (b) is the one that matters: at unit scope the strong guard had no
    reachable red, because every bot login also violated the weak shape.
    Row (a) additionally asserts that no USER LOOKUP was made, since the shape
    guard runs ahead of every lookup — without that, (a) and (b) are the same
    measurement twice. It is scoped to `/users*` rather than to the whole
    request log deliberately: the C-050 existing-root read legitimately precedes
    the owner ladder, so `requests == []` would assert an ordering the contract
    does not claim and red on a correct build.

    Mutations, one per row: narrow the login-shape predicate; delete the
    `identity.bot` check in `confirm_with_forge`; delete it in `seed_logins`.
    """
    seed_index_base(fake_forge)
    args = arrange(fake_forge)

    result = claim(ocx, fake_forge, *args)

    assert result.returncode == 64, result.stderr
    assert "--owner" in result.stderr, (
        f"64 is shared with six sibling variants; the remedy must be named: {result.stderr!r}"
    )
    if row == "weak-login-shape":
        assert [path for _, path in fake_forge.requests if path.startswith("/users")] == [], (
            "the login-shape guard runs before any user lookup: "
            f"{fake_forge.requests}"
        )


def test_empty_owner_login_refused_64(ocx: OcxRunner, fake_forge: FakeForge) -> None:
    """C-049 (hunt E-12): `--owner ""` is refused at **64** as an invalid login.

    The CLI deliberately does NOT filter an empty `--owner` value — treating it
    as absence would silently write whoever the CI environment names into a
    governance field — so it arrives as `OwnerSpec::Login("")` and the library
    owns the refusal. Nothing asserted that the library then actually refuses
    it, and the guard is one clause deep: `login_charset_is_valid` opens with
    `!login.is_empty()`.

    Asserted BY MESSAGE, not by code alone: 64 is shared with six sibling claim
    variants, so an exit-code-only row passes for the duplicate, the bot, the
    id-mismatch and the terminal-rung refusals equally. `invalid owner login`
    is `InvalidOwnerLogin`'s own sentence and no other variant's.

    The credential is present on purpose — the empty value is not an argv fault,
    so the run must get past `require_credential` (80) to reach the ladder at
    all.

    Mutation: drop the `!login.is_empty()` clause from `login_charset_is_valid`
    — the empty login then passes the charset guard, the confirm step cannot
    resolve it, and the run moves off 64 instead of writing an empty-login
    owner into a governance field.
    """
    seed_index_base(fake_forge)

    result = claim(ocx, fake_forge, "--owner", "")

    assert result.returncode == 64, result.stderr
    assert "invalid owner login" in result.stderr, (
        "64 is shared with six sibling variants, so the sentence is the row: "
        f"{result.stderr!r}"
    )


def test_owner_id_mismatch_64(ocx: OcxRunner, fake_forge: FakeForge) -> None:
    """C-048: `--owner alice:<wrong-id>` against a forge that reports another id
    is exit **64**, and the message names ALL THREE of the login, the supplied
    id and the actual id.

    64 is shared with six sibling claim variants, so an exit-code-only
    assertion passes for a message that names none of them — and the operator's
    next action depends on knowing which id the forge actually holds.

    Mutation: drop `actual` from the format string.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    result = claim(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:99")

    assert result.returncode == 64, result.stderr
    assert OWNER_LOGIN in result.stderr
    assert "99" in result.stderr, "the supplied id must be named"
    assert str(OWNER_ID) in result.stderr, "the id the forge actually holds must be named"


@pytest.mark.parametrize("seeded", [False, True])
def test_owner_unknown_79(ocx: OcxRunner, fake_forge: FakeForge, seeded: bool) -> None:
    """S-006/C-048: a login the forge does not know is **79**. TWO rows — the
    refusal and its positive control.

    79 is reachable here by accident: with `users` empty every claim run
    without `--owner` exits 79 (DX-74). The control is the SAME scenario with
    the login seeded, succeeding — that is what proves this row measured "the
    forge has no such account" rather than "the fixture was not seeded".

    Mutations: map `Ok(None)` to `NoActingIdentity` in `confirm_with_forge` —
    the refusal row reds; seed nothing — the control row reds.
    """
    seed_index_base(fake_forge)
    if seeded:
        fake_forge.seed_user("ghost", 11)

    result = claim(ocx, fake_forge, "--owner", "ghost")

    if seeded:
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)["owners"] == [{"login": "ghost", "id": 11}]
    else:
        assert result.returncode == 79, result.stderr
        assert "ghost" in result.stderr


def test_unseeded_token_identity_exits_79(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """Hunt E-01: omitting `seed_token_identity()` yields **79**, and the run
    that seeds it succeeds.

    This row exists so the seeding every no-`--owner` scenario performs is a
    DOCUMENTED precondition rather than a copied incantation. `users` starts
    empty while `GET /user` always answers `test-forge-bot`, so the token rung
    resolves a login the forge does not know and `confirm_with_forge` refuses
    it as an unknown owner — in a test that reads as if it had exercised the
    rung (DX-74).

    Both polarities in one row, over one fixture: without the seed the run is
    79 and names the token identity; with it, the same invocation succeeds and
    the owner list IS the token identity, which is the token rung actually
    doing its job.

    Mutation: return the seed unconfirmed from `confirm_with_forge`'s
    `Ok(None)` arm — the 79 half reds and the ladder silently records an
    account the forge does not have.
    """
    seed_index_base(fake_forge)

    unseeded = claim(ocx, fake_forge, "--out", str(tmp_path / "unseeded"))
    assert unseeded.returncode == 79, unseeded.stderr
    assert "test-forge-bot" in unseeded.stderr

    fake_forge.seed_token_identity()
    seeded = claim_json(ocx, fake_forge, "--out", str(tmp_path / "seeded"))
    assert seeded["owners"] == [{"login": "test-forge-bot", "id": 1001}]
    assert seeded["owner_identity_source"] == "resolved"


def test_no_acting_identity_64(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-007's error case: no `--owner`, no CI pair and no token identity is
    exit **64**, message naming `--owner`.

    Needs `fake_forge.token_identity_absent = True`. With the default fixture
    the identity endpoint always answers, so `NoActingIdentity` is UNREACHABLE
    and this named row would be dead — which is why the knob exists. The knob
    is narrow on purpose: `users_api_status` would also silence the owner
    lookup, and the ladder must still be able to reach it.

    Assert on the message, not the code alone: 64 is shared with six siblings,
    and the sentence naming `--owner` is the whole remedy.

    Mutation: map the absent identity to a `Forge` error instead — exit 80
    rather than 64.
    """
    seed_index_base(fake_forge)
    fake_forge.token_identity_absent = True

    result = claim(ocx, fake_forge, "--out", str(tmp_path / "out"))

    assert result.returncode == 64, result.stderr
    assert "--owner" in result.stderr, (
        f"the remedy must be named: {result.stderr!r}"
    )


def test_author_is_null_without_a_token_identity_or_a_ci_pair(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-060: `author` is **null** when neither rung can answer, with an
    explicit `--owner` still resolving normally.

    `resolve_author` is the token identity, else the CI pair, else `null` — the
    REVERSE of the seeding ladder, and it is consulted on every rung but the
    token one. `::test_claim_json_report_key_set` pins rung 1 (`test-forge-bot`
    / 1001); nothing pinned the terminal rung on the assembled document, which
    is where "when known" either renders `null` or quietly substitutes
    something that reads like an attestation. The owner list is a fourth
    distinct value (`alice` / 7), so a build that reused it as the author reds
    here instead of agreeing with itself.

    The row is also `token_identity_absent`'s positive control. That field
    exists ONLY because it is narrower than `users_api_status` — the latter
    silences all three user routes, which would take the owner lookup with it —
    and until this row nothing observed the difference: the other consumer
    (`::test_no_acting_identity_64`) fails at `seed_logins` before any lookup.
    Here the identity endpoint is out and the owner lookup still answers, which
    is the whole justification for the added field.

    Mutation: make `resolve_author`'s terminal fallthrough return a placeholder
    identity instead of `None` — this row reds and no rung-1 row moves, since
    those never reach the fallthrough.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)
    fake_forge.token_identity_absent = True

    report = claim_json(
        ocx,
        fake_forge,
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--out",
        str(tmp_path / "out"),
    )

    assert report["author"] is None, (
        "with no identity behind the credential and no CI pair, `author` is "
        "null rather than a stand-in the report's own doc warns is not an "
        "attestation"
    )
    assert report["owners"] == [{"login": OWNER_LOGIN, "id": OWNER_ID}], (
        "the positive control for `token_identity_absent`'s narrowness: the "
        "owner lookup still answered, so the run reached a resolved list rather "
        "than dying with the identity endpoint"
    )
    assert report["owner_identity_source"] == "resolved"


def test_owner_line_is_logged_and_filterable(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-072/DX-31 (settled by DX-75): the owner line is a library `log::info!`,
    present on a default run and absent under `--log-level error`.

    DX-31's residual — that the line is FILTERABLE, unlike the report key and
    the request body — is pinned behaviourally here rather than left in prose.
    Both polarities from one scenario, because each alone is vacuous: a
    presence assertion cannot tell the line from incidental stderr, and an
    absence assertion passes for a build that never emits it at all.

    The report and the exit code are identical either way, which is the other
    half of the contract: filtering a log line may not change what the command
    DID.

    Mutations: delete the `log::info!` — the presence half reds; make it an
    `eprintln!` — the absence half reds.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)
    owner_line = (
        f"claiming {CANONICAL_REGISTRY}/{PACKAGE} for {OWNER_LOGIN}:{OWNER_ID} "
        "(owner identity source: resolved)"
    )

    default_run = claim(
        ocx,
        fake_forge,
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--out",
        str(tmp_path / "default"),
    )
    quiet_run = claim(
        ocx,
        fake_forge,
        "--owner",
        f"{OWNER_LOGIN}:{OWNER_ID}",
        "--out",
        str(tmp_path / "quiet"),
        log_level="error",
    )

    assert default_run.returncode == 0, default_run.stderr
    assert quiet_run.returncode == 0, quiet_run.stderr
    assert owner_line in default_run.stderr, (
        f"C-072's owner line is missing from a default run: {default_run.stderr!r}"
    )
    assert owner_line not in quiet_run.stderr, (
        "the line is a `log::info!`, so `--log-level error` must silence it"
    )
    assert json.loads(default_run.stdout) == json.loads(quiet_run.stdout), (
        "filtering a log line may not change what the command reported"
    )


# ── usage refusals and argv ordering ──────────────────────────────────────


def test_no_credential_exits_80(ocx: OcxRunner, fake_forge: FakeForge) -> None:
    """C-063: no `OCX_ANNOUNCE_TOKEN` and no `--out` is exit **80**, the message
    naming both the variable and `--out`, with `fake_forge.requests == []`.

    The zero-calls half is what names the contract. The refusal precedes forge
    construction — and with it deleted the run would reach the fake, which
    enforces no authentication, and **succeed**; an exit-code-only assertion
    would then red on a success rather than on the missing refusal.

    Mutation: delete the `require_credential` call — the run exits 0 and writes
    a claim with no credential.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    result = claim(ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}", token=None)

    assert result.returncode == 80, result.stderr
    assert "OCX_ANNOUNCE_TOKEN" in result.stderr
    assert "--out" in result.stderr, "the message names the mode that works without one"
    assert fake_forge.requests == [], (
        f"the refusal precedes forge construction: {fake_forge.requests}"
    )


def test_malformed_repository_exits_64_without_a_credential(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """C-065/S-002: a malformed `--repository` is **64** even with no credential
    present — the argv faults outrank the credential refusal.

    This is the only scope that can red that ordering. The CLI parses
    `--repository` in its argv-fault block and discards the value, BEFORE
    resolving credentials; a unit test can prove the call exists but not that
    it outranks the refusal, because the command's `execute` installs a
    process-global subscriber and is not unit-callable.

    `token=None` is load-bearing, not tidiness: with a credential present both
    orderings exit 64 and the row proves nothing. `requests == []` separates
    64-before-I/O from 64-after.

    Mutation: move the repository parse out of the argv-fault block, or below
    the credential refusal — the run exits **80**.
    """
    seed_index_base(fake_forge)

    result = claim(ocx, fake_forge, repository="not-a-url", token=None)

    assert result.returncode == 64, result.stderr
    assert "not-a-url" in result.stderr, "the refused value must be named"
    assert fake_forge.requests == [], (
        f"an argv fault precedes every forge call: {fake_forge.requests}"
    )


@pytest.mark.parametrize(
    ("flag", "value"),
    [
        ("--upstream-repository-url", "https://acme.invalid/widget"),
        ("--upstream-disclaimer", "repackaged by Acme"),
        ("both", ""),
    ],
)
def test_upstream_flags_require_the_anchor(
    ocx: OcxRunner, fake_forge: FakeForge, flag: str, value: str
) -> None:
    """S-012/C-057 (hunt E-33): each `--upstream-*` satellite requires
    `--upstream-org`, and the refusal is clap's, at **64**.

    Three refusals: each satellite alone, and both together without the anchor.
    The accepted case is its own row below — every named row in the plan is a
    refusal, so a `requires` written in the wrong DIRECTION refuses S-012 and
    reds nothing named.

    Mutation: delete `requires = "upstream_org"` from either flag — one refusal
    row reds.
    """
    seed_index_base(fake_forge)
    args = (
        ["--upstream-repository-url", "https://acme.invalid/widget", "--upstream-disclaimer", "x"]
        if flag == "both"
        else [flag, value]
    )

    result = claim(ocx, fake_forge, *args, token=None)

    assert result.returncode == 64, result.stderr
    assert "--upstream-org" in result.stderr, (
        f"the refusal must name the anchor: {result.stderr!r}"
    )


def test_upstream_org_alone_reaches_the_root(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """S-012 (hunt E-33): `--upstream-org "Acme Org"` alone succeeds and the
    root's `upstream` carries ONLY `org`.

    The accepted half of the row above, and the one that proves the `requires`
    direction: an anchor that itself required a satellite would refuse this and
    red nothing the plan names.

    Asserted as equality on the whole object — an `upstream` that also emitted
    `repository_url: null` and `disclaimer: null` satisfies
    `upstream["org"] == "Acme Org"` while changing the document every index
    consumer reads.

    Mutation: invert the `requires` direction; or emit the absent satellites as
    `null`.
    """
    seed_index_base(fake_forge)
    fake_forge.seed_user(OWNER_LOGIN, OWNER_ID)

    claim_json(
        ocx, fake_forge, "--owner", f"{OWNER_LOGIN}:{OWNER_ID}", "--upstream-org", "Acme Org"
    )

    root = json.loads(claimed_root_bytes(fake_forge))
    assert root["upstream"] == {"org": "Acme Org"}


@pytest.mark.parametrize(
    ("row", "url"),
    [
        ("non-http-scheme", "ftp://acme.invalid/widget"),
        ("embedded-userinfo", "https://ci:glpat-SECRET-JOB-TOKEN@acme.invalid/widget"),
    ],
)
def test_upstream_repository_url_refusals(
    ocx: OcxRunner, fake_forge: FakeForge, row: str, url: str
) -> None:
    """DX-64 (hunt E-34): `--upstream-repository-url` must be an absolute
    `http`/`https` URL with no userinfo, refused at **64** otherwise.

    The URL reaches the index root by design and `ocx-catalog` renders it as a
    link, so an unvalidated scheme is a stored-link vector no ocx-side control
    covers.

    Both rows additionally assert the message does **not** echo the refused
    value. The likely input is a forwarded `CI_REPOSITORY_URL` whose userinfo
    is a live job token, so echoing it is CWE-532 — and the sibling
    `parse_owner_spec` refusal DOES echo its value, so a builder normalising
    the two messages introduces the leak. Nothing else asserts the omission.

    Mutations: add `{url}` to the message in
    `validate_upstream_repository_url` — both rows red; drop the userinfo
    clause in `upstream_repository_url_is_publishable` — the second row reds.
    """
    seed_index_base(fake_forge)

    result = claim(
        ocx,
        fake_forge,
        "--upstream-org",
        "Acme Org",
        "--upstream-repository-url",
        url,
        token=None,
    )

    assert result.returncode == 64, result.stderr
    assert "--upstream-repository-url" in result.stderr, (
        f"the refusal must name the flag: {result.stderr!r}"
    )
    assert url not in result.stderr, (
        f"the refused value must not be echoed (CWE-532): {result.stderr!r}"
    )
    assert "SECRET-JOB-TOKEN" not in result.stderr, (
        "no fragment of the userinfo may reach any log surface"
    )


def test_transport_git_on_github_64(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-019/C-058: `--transport git` against a resolved GitHub forge is **64**,
    naming the forge and the flag.

    Run under a `PATH` holding no `git` at all. On any host that HAS git the
    row passes whichever order the two checks sit in; only an absent binary
    separates "validated the transport" (64) from "probed first" (69) — which
    is the ordering C-065 contracts.

    Mutation: move the git probe above the argv-fault block — 69 instead of 64.
    """
    shim = install_git_shim(tmp_path, variant=GitShimVariant.ABSENT)
    seed_index_base(fake_forge)

    result = claim(ocx, fake_forge, "--transport", "git", path=shim.child_path())

    assert result.returncode == 64, result.stderr
    assert "github" in result.stderr.lower(), "the message names the forge"
    assert "--transport" in result.stderr, "the message names the flag"


def _assert_transport_git_conflict(
    result, other_flag: str
) -> None:
    """S-020/C-058's shared assertion: **64**, message naming BOTH flags.

    A message naming one flag exits 64 too, and the two refusals are two
    separate string literals (`options/forge_write.rs`) — so the message is the
    row, not the code. Shared as a helper rather than as a parametrised test so
    the plan's two names both survive as functions a reader can grep for.
    """
    assert result.returncode == 64, result.stderr
    assert "--transport" in result.stderr, "the message names the transport flag"
    assert other_flag in result.stderr, f"the message must also name {other_flag}"


def test_transport_git_with_fork_64(
    ocx: OcxRunner, fake_forge: FakeForge
) -> None:
    """S-020/C-058: `--transport git` with `--fork` is **64**, message naming
    BOTH flags.

    The forge is GitLab so the GitHub refusal above cannot shadow this one:
    `--transport git` against a resolved GitHub forge is its own 64, and a
    GitHub scenario here would pass while proving nothing about this literal.

    Mutation: drop `--fork` from the message.
    """
    seed_index_base(fake_forge)

    result = claim(
        ocx,
        fake_forge,
        "--transport",
        "git",
        "--fork",
        f"forkuser/{INDEX_REPO}",
        forge="gitlab",
    )

    _assert_transport_git_conflict(result, "--fork")


def test_transport_git_with_out_64(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-020/C-058: `--transport git` with `--out` is **64**, message naming
    BOTH flags. The sibling literal of the row above.

    Mutation: drop `--out` from the message.
    """
    seed_index_base(fake_forge)

    result = claim(
        ocx,
        fake_forge,
        "--transport",
        "git",
        "--out",
        str(tmp_path / "out"),
        forge="gitlab",
    )

    _assert_transport_git_conflict(result, "--out")


# ── the argv-boundary git gate (C-065, C-075) ─────────────────────────────


def test_missing_git_binary_69_with_zero_network_calls(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-021/C-065: `--transport git` on a host with no `git` is **69** before
    any forge call.

    Four assertions, because exit 69 alone passes for a CLI that never probed
    and let the forge constructor raise it (the constructor returns the same
    code):

    1. exit **69**;
    2. `fake_forge.requests == []` — zero REST calls;
    3. `fake_forge.git_http_requests == []` — the git transport keeps a SEPARATE
       log, dispatched before the REST recorder, so asserting only `requests`
       would be blind to a run that reached the git bridge;
    4. stderr carries the PROBE's message, never the constructor's.

    Runs against a **GitLab** forge: `--transport git` on GitHub is refused at
    validation before the probe ever runs (see the row above), so a GitHub
    scenario would exit 64 and never reach this contract.

    **The socket half is the harness's, not claim's** (DX-62/DX-73). The
    ambient self-update check precedes every command and stays silent only
    because `capture_output=True` makes stderr a pipe; `env::is_ci()` is not a
    live second guard, since `CI` is off the child-environment whitelist. A
    future fixture that gave `ocx` a TTY would flip that for a reason unrelated
    to claim. The guarantee claim's own code makes is zero FORGE calls, which
    is what rows 2 and 3 assert.

    Mutation: pass `None` for the git binary instead of probing — the exit code
    is unchanged and only the message assertion reds.
    """
    shim = install_git_shim(tmp_path, variant=GitShimVariant.ABSENT)
    seed_index_base(fake_forge)

    result = claim(
        ocx, fake_forge, "--transport", "git", forge="gitlab", path=shim.child_path()
    )

    assert result.returncode == 69, result.stderr
    assert fake_forge.requests == [], f"a REST call was made: {fake_forge.requests}"
    assert fake_forge.git_http_requests == [], (
        f"the run reached the git bridge: {fake_forge.git_http_requests}"
    )
    assert "git could not be resolved on PATH" in result.stderr, (
        "the PROBE's message, not `ForgeKind::client`'s constructor message — a "
        f"CLI that passed None instead of probing exits 69 too: {result.stderr!r}"
    )


def _claim_over_git(
    ocx: OcxRunner,
    fake_forge: FakeForge,
    tmp_path: Path,
    variant: GitShimVariant,
    extra_env: dict[str, str] | None = None,
):
    """One `--transport git` claim against a GitLab forge, under a `git` whose
    reported version the shim spoofs.

    The fixture state is shared by both version rows and is what makes them one
    measurement in two directions: the root is **already committed**, so a run
    that gets past the probe refuses at 65 on the C-050 read. That is a positive
    observation of "the probe was passed" which stops short of the push —
    everything past the read is `test_transport_git.py`'s (WP-17), and a row
    that needed the push to succeed would red on WP-17's surface.

    GitLab, because `--transport git` on GitHub is refused at validation before
    the probe ever runs.
    """
    shim = install_git_shim(tmp_path, variant=variant)
    seed_claimed_root(fake_forge)
    return claim(
        ocx,
        fake_forge,
        "--transport",
        "git",
        forge="gitlab",
        path=shim.child_path(),
        extra_env=extra_env,
    )


def test_git_below_the_floor_exits_69(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-075/DX-18: a `git` reporting **2.30.9** is refused at **69** with
    `fake_forge.requests == []`.

    The refuse side of the version gate, driven by the shim's version variant.
    The unit tests parse a version STRING and never resolve a real `git` off
    `PATH`, so nothing else exercises the shim's version variants end to end.

    Mutation: accept everything — this row reds.
    """
    result = _claim_over_git(ocx, fake_forge, tmp_path, GitShimVariant.VERSION_2_30_9)

    assert result.returncode == 69, result.stderr
    assert fake_forge.requests == [], (
        f"a refused version must not reach the forge: {fake_forge.requests}"
    )


def test_git_at_the_floor_is_accepted(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-075/DX-18: a `git` reporting exactly **2.31.0** is ACCEPTED.

    The accept side needs a positive, because a gate that refuses everything
    passes every refusal test — and "the exit code is not 69" is satisfied by
    any later failure, including a crash. The discriminator is that the run got
    PAST the probe: the C-050 root read happened, and because the fixture has
    the root committed the run then refuses at **65** — an exit code that only
    the read can produce.

    Mutation: compare versions with the semver type instead of the tuple —
    2.31.0 is then refused and this row reds.
    """
    result = _claim_over_git(ocx, fake_forge, tmp_path, GitShimVariant.VERSION_2_31_0)

    assert result.returncode == 65, (
        "2.31.0 is the boundary release the floor is named for and must be "
        f"accepted; the run then refuses on the committed root: {result.stderr!r}"
    )
    assert any("/repository/files/" in path for _, path in fake_forge.requests), (
        f"the C-050 root read never happened, so the probe was not passed: {fake_forge.requests}"
    )


# ── C-064's push-identity notice, the claim half ──────────────────────────

#: A job token that is NOT `TOKEN`, so a run carrying both is unambiguously
#: "the operator's own credential, inside a job that has its own". Without the
#: distinction `push_is_job_token()` holds and the notice correctly stays quiet,
#: which would make the positive row below green for the wrong reason.
JOB_TOKEN = "glcbt_test_job_token_CLAIM_JOB_TOKEN_1234567890"

#: The distinguishing substring of the notice
#: (`options::ForgeWriteOptions::warn_push_identity`). Long enough to be that
#: sentence and nothing else, short enough to survive a reworded remedy clause.
PUSH_IDENTITY_NEEDLE = "not this job's CI_JOB_TOKEN"

#: `GitPushCredential::DEFAULT_USERNAME` -- the user half of the HTTP Basic pair
#: `--transport git` sends, whose value GitLab ignores. Spelled here because it
#: is the string DX-86 says must NOT reach stderr; the Rust-side pin that reads
#: it off the constant is
#: `options::forge_write::tests::the_push_identity_notice_names_no_person`.
PUSH_BASIC_USERNAME = "gitlab-ci-token"


def test_git_transport_warns_that_the_operator_token_authors_the_request(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-064, the claim half: inside `GITLAB_CI`, with a non-job
    `OCX_ANNOUNCE_TOKEN` and no `OCX_ANNOUNCE_GIT_TOKEN`, `ocx package claim`
    emits the same one stderr line `ocx package announce` does.

    The state is the one C-063's push ladder produces silently, and claim reaches
    it through the identical `ForgeCredentials::resolve`: rung 1 finds no
    `OCX_ANNOUNCE_GIT_TOKEN`, so rung 2 hands the push half a copy of the API
    credential the operator set for the REST calls, and the merge request is
    authored by that token's owner instead of by the pipeline. No exit code shows
    that -- which is why claim carrying no emitter was a real gap and not a
    stylistic one.

    Rides `VERSION_2_31_0`, so the run gets past the version probe and refuses at
    **65** on the C-050 read of the already-committed root. That exit is asserted:
    it proves the notice was emitted on the path a real write takes, not by a run
    that died before reaching the emit site.

    DX-86: it names the credential, never a person. The one identity-shaped
    string ocx holds here is the HTTP Basic username, which in this exact state
    is the constant `gitlab-ci-token` -- it names the pipeline, the very party
    this notice says did NOT author the request.

    Mutation: delete the `self.forge.warn_push_identity(...)` call in
    `package_claim.rs`; every stderr assertion below reds while the announce
    twin in `test_announce_gitlab.py` stays green.
    """
    result = _claim_over_git(
        ocx,
        fake_forge,
        tmp_path,
        GitShimVariant.VERSION_2_31_0,
        extra_env={"GITLAB_CI": "true", "CI_JOB_TOKEN": JOB_TOKEN},
    )

    assert result.returncode == 65, (
        "the notice is a diagnostic, and the run must still reach the C-050 read "
        f"so the emit sits on the path a write takes: {result.stderr!r}"
    )
    assert result.stderr.count(PUSH_IDENTITY_NEEDLE) == 1, (
        f"exactly one notice, naming the credential the push does NOT use: {result.stderr!r}"
    )
    assert "push credential kind token" in result.stderr, (
        f"the notice names the push credential kind the report also carries: {result.stderr!r}"
    )
    assert "OCX_ANNOUNCE_TOKEN" in result.stderr, (
        f"the notice names the variable the push secret came from: {result.stderr!r}"
    )
    assert "OCX_ANNOUNCE_GIT_TOKEN" in result.stderr, (
        f"the notice names the variable that changes the outcome: {result.stderr!r}"
    )
    assert PUSH_BASIC_USERNAME not in result.stderr, (
        "DX-86: the HTTP Basic username names the pipeline, the one party this "
        f"notice says did not author the request: {result.stderr!r}"
    )
    assert "cannot name" in result.stderr, (
        f"ocx must say it cannot identify the token's owner: {result.stderr!r}"
    )
    # X6: the notice talks about credentials and may carry neither of them.
    assert TOKEN not in result.stderr and JOB_TOKEN not in result.stderr, (
        f"no credential value may reach stderr: {result.stderr!r}"
    )
    # S-034: a diagnostic on stderr leaves stdout a single parseable document.
    assert PUSH_IDENTITY_NEEDLE not in result.stdout, (
        f"the notice must never reach stdout: {result.stdout!r}"
    )


def test_the_same_claim_outside_gitlab_ci_says_nothing(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """C-064, the claim negative half: the identical run without `GITLAB_CI`
    emits nothing.

    This is the half that makes the pair discriminating. `OCX_ANNOUNCE_TOKEN`
    reaching the push half is the ordinary, correct outcome outside a CI job --
    there is no job token it displaced, and the operator is authenticating as
    themselves on purpose. A notice that fired here would satisfy the sibling
    above just as well while telling every local operator to fix a run behaving
    exactly as asked.

    `CI_JOB_TOKEN` stays set, so `GITLAB_CI` is the ONLY difference between the
    two runs and the assertion cannot be satisfied by the two environments
    differing somewhere else. The shared exit-65 assertion is what rules out the
    other way to pass silently: a run that failed before the emit site would also
    print no notice.

    Mutation: drop the `credentials.in_gitlab_ci()` conjunct from the guard in
    `options/forge_write.rs` -- this reds while the sibling above stays green.
    """
    result = _claim_over_git(
        ocx,
        fake_forge,
        tmp_path,
        GitShimVariant.VERSION_2_31_0,
        extra_env={"CI_JOB_TOKEN": JOB_TOKEN},
    )

    assert result.returncode == 65, (
        f"the run must reach the same emit site as its in-CI twin: {result.stderr!r}"
    )
    assert PUSH_IDENTITY_NEEDLE not in result.stderr, (
        f"outside a GitLab job there is no displaced job token to warn about: {result.stderr!r}"
    )
    assert "push credential kind" not in result.stderr, (
        f"the notice must not fire outside GITLAB_CI at all: {result.stderr!r}"
    )


# ── the announce side of the pair (S-005, S-037) ──────────────────────────


def test_announce_unclaimed_namespace_exits_79(
    ocx: OcxRunner, fake_forge: FakeForge, tmp_path: Path
) -> None:
    """S-005/S-037: `ocx package announce acme/widget` on an UNCLAIMED namespace
    is still **79**, and the message is the unclaimed-namespace one.

    The signal a release wrapper branches on: 79 from announce means "claim
    first", 65 from claim means "already claimed, go announce". Two different
    errors share 79 across the two commands — `UnclaimedNamespace` here and
    `OwnerUnknown` in the claim rows above — so the message assertion is what
    rules out the confusion the pair exists to prevent, and an exit-code-only
    row would pass for either.

    **Written in the post-WP-15 grammar**: the package is a POSITIONAL. The
    deprecated `--package` form is deliberately absent — it would be a
    hundredth in-repository invocation that WP-18's repo-wide check reds on.

    Mutation: reclassify `UnclaimedNamespace` as a data error — the code moves
    off 79.
    """
    seed_index_base(fake_forge)

    result = announce(
        ocx,
        fake_forge,
        PACKAGE,
        "--tags",
        "1.0.0",
        "--out",
        str(tmp_path / "out"),
        check=False,
    )

    assert result.returncode == 79, result.stderr
    assert "claim" in result.stderr.lower(), (
        "the message must be the unclaimed-namespace one, not `OwnerUnknown`'s: "
        f"{result.stderr!r}"
    )
