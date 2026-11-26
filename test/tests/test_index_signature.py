# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the signed index catalog check on the remote-to-local path.

A served index carries `c/index.json` and a DSSE bundle `c/index.json.sigstore.json` made by
`ocx index sign`. A registry's `index_signature` (`off`, `warn`, `enforce`) decides what a missing,
forged, foreign, expired or stale signature does to `ocx index update`, `ocx update` and
`ocx package prune`. Each case routes a registry to a local `StaticIndexServer` signed with a
per-test cosign key; the server's request log proves how many fetches a process made.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import previous_release, static_index
from src.helpers import make_package, push_managed_config
from src.index_signing import (
    CATALOG,
    CATALOG_URL_PATH,
    SIGNATURE,
    SIGNATURE_URL_PATH,
    Key,
    assert_refused,
    authority,
    configure_index,
    index_warnings,
    keyless_entry,
    make_key,
    sign_catalog_beyond_the_cli_cap,
    sign_index,
    stack_table,
    write_index,
)
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner
from tests.fixtures.sigstore_stack import SigstoreStack

pytestmark = pytest.mark.command(
    "index_update*", "index_sync*", "index_catalog*", "update", "package_prune*"
)


@pytest.fixture()
def key_a(tmp_path: Path) -> Key:
    return make_key(tmp_path / "key_a")


@pytest.fixture()
def key_b(tmp_path: Path) -> Key:
    return make_key(tmp_path / "key_b")


@pytest.fixture()
def server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as running:
        yield running


def serve(
    ocx: OcxRunner,
    server: static_index.StaticIndexServer,
    key: Key,
    unique_repo: str,
    *,
    mode: str = "enforce",
    count: int = 1,
    days: int | None = None,
    registry: str = "ocx.sh",
    sign_registry: str | None = None,
    **config: object,
) -> list[str]:
    """Serve a signed index of `count` packages, trust `key`, and return the repository names."""
    repositories = [f"{unique_repo}/pkg{position}" for position in range(count)]
    write_index(server.root, repositories, registry=registry)
    sign_index(
        ocx,
        server.root,
        registry=sign_registry or registry,
        key=key,
        expires_in_days=days,
    )
    configure_index(
        ocx,
        server,
        registry=registry,
        index_signature=mode,
        index_publishers=[key.entry],
        **config,  # type: ignore[arg-type]
    )
    return repositories


def update(
    ocx: OcxRunner,
    repositories: list[str],
    *flags: str,
    registry: str = "ocx.sh",
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    identifiers = [f"{registry}/{repository}" for repository in repositories]
    return ocx.run(
        "index", "update", *flags, *identifiers, check=False, env_overlay=env
    )


def break_signature(
    case: str, ocx: OcxRunner, server: static_index.StaticIndexServer, key_b: Key
) -> None:
    """Make the served signature missing, wrong for the served bytes, or from a signer outside the list."""
    if case == "missing":
        (server.root / SIGNATURE).unlink()
    elif case == "tampered":
        catalog = server.root / CATALOG
        catalog.write_bytes(catalog.read_bytes() + b"\n")
    else:
        sign_index(ocx, server.root, key=key_b)


FAILURES = [
    ("missing", 79, "index_signature_not_found"),
    ("tampered", 65, "index_signature_invalid"),
    # A key signature carries no public key, only a hint: an unlisted key and a forged signature
    # are the same refusal, as they are for `ocx package verify`.
    ("other_signer", 65, "index_signature_invalid"),
]


# ──────────────────────────────────────────────────────────────────────────────
# enforce
# ──────────────────────────────────────────────────────────────────────────────


def test_enforce_accepts_a_catalog_signed_by_a_listed_key_with_one_fetch_per_process(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo, count=2)

    result = update(ocx, repositories)

    assert result.returncode == 0, result.stderr
    assert server.count(CATALOG_URL_PATH) == 1, (
        "one catalog fetch serves every root of the process"
    )
    assert server.count(SIGNATURE_URL_PATH) == 1, (
        "one signature fetch serves every root of the process"
    )


def test_enforce_installs_a_package_from_a_signed_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key
) -> None:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")
    repository = f"{unique_repo}/pkg"
    static_index.write_config(server.root)
    entry = static_index.write_package(
        server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf,
        os=os_name,
        architecture=arch_name,
    )
    static_index.write_catalog(server.root, {repository: entry.root_digest})
    sign_index(ocx, server.root, key=key_a)
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )

    result = ocx.run("package", "install", entry.logical_id, check=False)

    assert result.returncode == 0, result.stderr
    assert list((ocx.ocx_home / "packages").rglob("metadata.json")), (
        "the package was not installed"
    )


def test_enforce_refuses_a_keyless_signer_the_list_does_not_name_as_a_publisher_mismatch(
    ocx: OcxRunner,
    unique_repo: str,
    server,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    repositories = [f"{unique_repo}/pkg0"]
    write_index(server.root, repositories)
    configure_index(
        ocx,
        server,
        index_signature="enforce",
        index_publishers=[
            keyless_entry(sigstore_stack, identity="someone-else@example.com")
        ],
        extra_toml=stack_table(sigstore_stack),
    )
    sign_index(ocx, server.root, stack=sigstore_stack, token=identity_token)

    assert_refused(update(ocx, repositories), 77, "index_publisher_mismatch")


def test_the_trusted_root_variable_does_not_supply_the_root_of_the_catalog_check(
    ocx: OcxRunner,
    unique_repo: str,
    server,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """The variable alone cannot satisfy the catalog check; a table root or the convention file can.

    1. Catalog signed by the private instance, whose table carries no root, with the stack's root in
       `OCX_SIGSTORE_TRUSTED_ROOT` only: refused 65 `index_signature_invalid`, byte-identical to the run with it unset.
    2. Positive control: the same root in the instance's `trusted_root` makes the update pass, so the
       root itself is good.
    3. The root at the instance's convention path, bare table, variable unset: the update passes.
    """
    repositories = [f"{unique_repo}/pkg0"]
    name = authority(sigstore_stack)
    write_index(server.root, repositories)
    publishers = [keyless_entry(sigstore_stack)]
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=publishers,
        extra_toml=stack_table(sigstore_stack),
    )
    sign_index(ocx, server.root, stack=sigstore_stack, token=identity_token)
    bare = f'[sigstore."{name}"]\nfulcio_url = "{sigstore_stack.fulcio_url}"\nrekor_url = "{sigstore_stack.rekor_url}"\n'
    configure_index(ocx, server, index_signature="enforce", index_publishers=publishers, extra_toml=bare)
    variable = {"OCX_SIGSTORE_TRUSTED_ROOT": str(sigstore_stack.trusted_root_json)}

    without_root = update(ocx, repositories)
    assert_refused(without_root, 65, "index_signature_invalid")
    assert f"no trusted root for Sigstore instance `{name}`" in without_root.stdout, without_root.stdout
    with_variable = update(ocx, repositories, env=variable)
    assert_refused(with_variable, 65, "index_signature_invalid")
    assert with_variable.stdout == without_root.stdout, "the variable must change nothing about the refusal"

    configure_index(
        ocx, server, index_signature="enforce", index_publishers=publishers,
        extra_toml=stack_table(sigstore_stack),
    )
    control = update(ocx, repositories)
    assert control.returncode == 0, control.stderr

    shutil.rmtree(ocx.ocx_home / "state" / "trust_root", ignore_errors=True)
    configure_index(ocx, server, index_signature="enforce", index_publishers=publishers, extra_toml=bare)
    convention = ocx.ocx_home / "sigstore" / "instances" / name.replace(":", "_") / "trusted-root.json"
    convention.parent.mkdir(parents=True)
    convention.write_bytes(sigstore_stack.trusted_root_json.read_bytes())
    installed = update(ocx, repositories)
    assert installed.returncode == 0, installed.stderr


@pytest.mark.parametrize(("case", "code", "slug"), FAILURES)
def test_enforce_refuses_a_bad_signature_with_its_slug(
    ocx: OcxRunner,
    unique_repo: str,
    server,
    key_a: Key,
    key_b: Key,
    case: str,
    code: int,
    slug: str,
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo)
    break_signature(case, ocx, server, key_b)

    assert_refused(update(ocx, repositories), code, slug)


@pytest.mark.parametrize("absent", [SIGNATURE, CATALOG])
def test_enforce_refuses_a_remote_catalog_listing_whose_signature_or_catalog_is_absent(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, absent: str
) -> None:
    serve(ocx, server, key_a, unique_repo)
    (server.root / absent).unlink()

    result = ocx.run("--remote", "index", "catalog", "ocx.sh", check=False)

    assert_refused(result, 79, "index_signature_not_found")
    assert unique_repo not in result.stdout, "the unsigned catalog was listed"


@pytest.mark.parametrize("absent", [SIGNATURE, CATALOG])
def test_enforce_refuses_an_index_sync_whose_signature_or_catalog_is_absent(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, absent: str
) -> None:
    serve(ocx, server, key_a, unique_repo)
    (server.root / absent).unlink()

    result = ocx.run("index", "sync", "ocx.sh", check=False)

    assert_refused(result, 79, "index_signature_not_found")
    index_home = Path(ocx.env["OCX_HOME"]) / "index"
    recorded = [path for path in index_home.rglob("*") if unique_repo in str(path)]
    assert not recorded, f"nothing from the unsigned catalog is recorded: {recorded}"


def test_enforce_refuses_an_expired_catalog_until_expiry_is_ignored(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo, days=0)
    assert_refused(update(ocx, repositories), 81, "index_catalog_expired")

    configure_index(
        ocx,
        server,
        index_signature="enforce",
        index_publishers=[key_a.entry],
        index_expiry="ignore",
    )
    assert update(ocx, repositories).returncode == 0


def test_enforce_accepts_a_hand_minted_bundle_expiring_inside_the_cap(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    """The control for the refusal below: the same minting at 399 days verifies, so only the expiry differs."""
    repositories = [f"{unique_repo}/pkg0"]
    write_index(server.root, repositories)
    sign_catalog_beyond_the_cli_cap(ocx, server.root, key_a, expires_in_days=399)
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )

    result = update(ocx, repositories)

    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize("expiry", ["check", "ignore"])
def test_enforce_refuses_a_catalog_expiring_past_the_cap_even_when_signed_by_a_listed_key(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, expiry: str
) -> None:
    repositories = [f"{unique_repo}/pkg0"]
    write_index(server.root, repositories)
    sign_catalog_beyond_the_cli_cap(ocx, server.root, key_a, expires_in_days=500)
    configure_index(
        ocx,
        server,
        index_signature="enforce",
        index_publishers=[key_a.entry],
        index_expiry=expiry,
    )

    assert_refused(update(ocx, repositories), 65, "index_signature_invalid")


def test_enforce_refuses_a_root_whose_bytes_differ_from_the_catalog_digest_and_commits_nothing(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    [repository] = serve(ocx, server, key_a, unique_repo)
    root = server.root / "p" / f"{repository}.json"
    root.write_text(json.dumps(json.loads(root.read_text()), indent=1))

    assert_refused(update(ocx, [repository]), 65, "index_root_digest_mismatch")
    committed = [
        path
        for path in (ocx.ocx_home / "index").rglob("*")
        if unique_repo in path.as_posix()
    ]
    assert committed == [], f"a refused root must not be committed locally: {committed}"


def test_enforce_refuses_a_root_of_a_package_the_catalog_does_not_list(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    [listed, unlisted] = [f"{unique_repo}/pkg{position}" for position in range(2)]
    entries = write_index(server.root, [listed, unlisted])
    static_index.write_catalog(server.root, {listed: entries[listed].root_digest})
    sign_index(ocx, server.root, key=key_a)
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )

    assert_refused(update(ocx, [unlisted]), 79, "index_package_not_in_catalog")


def test_enforce_refuses_a_listed_root_the_server_does_not_have_and_never_falls_through(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    [repository] = serve(ocx, server, key_a, unique_repo)
    (server.root / "p" / f"{repository}.json").unlink()

    assert_refused(update(ocx, [repository]), 79, "index_root_not_found")


def test_enforce_refetches_once_when_the_first_signature_belongs_to_the_previous_catalog(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo, days=30)
    stale = (server.root / SIGNATURE).read_bytes()
    sign_index(ocx, server.root, key=key_a, expires_in_days=60)
    server.serve_stale_first(SIGNATURE_URL_PATH, stale)

    result = update(ocx, repositories)

    assert result.returncode == 0, result.stderr
    assert server.count(SIGNATURE_URL_PATH) == 2, "exactly one refetch of the signature"
    assert server.count(CATALOG_URL_PATH) == 2, "the refetch takes the catalog again"


def test_enforce_judges_concurrent_roots_against_one_snapshot_across_a_generation_switch(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key
) -> None:
    repositories = [f"{unique_repo}/pkg{position}" for position in range(2)]
    old = tmp_path / "generation_old"
    old.mkdir()
    write_index(old, repositories)
    static_index.write_catalog(
        old, {name: "sha256:" + "0" * 64 for name in repositories}
    )
    sign_index(ocx, old, key=key_a, expires_in_days=30)
    write_index(server.root, repositories)
    sign_index(ocx, server.root, key=key_a, expires_in_days=60)
    server.scripted_bodies[CATALOG_URL_PATH] = [
        (old / CATALOG).read_bytes(),
        (server.root / CATALOG).read_bytes(),
    ]
    server.scripted_bodies[SIGNATURE_URL_PATH] = [
        (old / SIGNATURE).read_bytes(),
        (server.root / SIGNATURE).read_bytes(),
    ]
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )

    result = update(ocx, repositories)

    assert result.returncode == 0, (
        f"both roots must verify against the new generation\n{result.stderr}"
    )
    assert server.count(CATALOG_URL_PATH) == 2, (
        "one refetch for both concurrent roots, never one each"
    )
    assert server.count(SIGNATURE_URL_PATH) == 2


def test_enforce_refuses_a_valid_older_catalog_after_a_newer_one_and_warn_only_warns(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo, days=60)
    assert update(ocx, repositories).returncode == 0
    sign_index(ocx, server.root, key=key_a, expires_in_days=10)

    assert_refused(update(ocx, repositories), 65, "index_catalog_rollback")

    configure_index(ocx, server, index_signature="warn", index_publishers=[key_a.entry])
    warned = update(ocx, repositories)
    assert warned.returncode == 0, warned.stderr
    assert len(index_warnings(warned)) == 1, warned.stderr


def test_enforce_warns_once_when_the_rollback_record_cannot_be_kept_and_still_updates(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo, days=10)
    assert update(ocx, repositories).returncode == 0
    records = Path(ocx.env["OCX_HOME"]) / "state" / "index_catalog"
    shutil.rmtree(records)
    records.write_text("not a directory")
    sign_index(ocx, server.root, key=key_a, expires_in_days=60)

    result = update(ocx, repositories)

    assert result.returncode == 0, result.stderr
    lines = [line for line in result.stderr.splitlines() if "expiry record" in line]
    assert len(lines) == 1, result.stderr
    assert str(records) in lines[0], lines[0]


def test_enforce_fails_with_the_transport_error_when_the_signature_endpoint_answers_500(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo)
    server.refusals[SIGNATURE_URL_PATH] = 500

    result = update(ocx, repositories)

    assert result.returncode == 69, result.stdout + result.stderr


def test_enforce_without_a_signer_list_is_a_config_error(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    [repository] = serve(ocx, server, key_a, unique_repo, registry="idx.example")
    configure_index(ocx, server, registry="idx.example", index_signature="enforce")

    assert_refused(
        update(ocx, [repository], registry="idx.example"),
        78,
        "index_publishers_missing",
    )


# ──────────────────────────────────────────────────────────────────────────────
# warn
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize(("case", "code", "slug"), FAILURES)
def test_warn_logs_one_warning_per_source_and_the_command_succeeds(
    ocx: OcxRunner,
    unique_repo: str,
    server,
    key_a: Key,
    key_b: Key,
    case: str,
    code: int,
    slug: str,
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo, mode="warn", count=2)
    break_signature(case, ocx, server, key_b)

    result = update(ocx, repositories)

    assert result.returncode == 0, (
        f"warn must not fail ({case}, would be {code} {slug})\n{result.stderr}"
    )
    assert len(index_warnings(result)) == 1, (
        f"exactly one WARN per source:\n{result.stderr}"
    )


def test_warn_survives_a_signature_endpoint_that_answers_500_with_one_warning(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo, mode="warn")
    server.refusals[SIGNATURE_URL_PATH] = 500

    result = update(ocx, repositories)

    assert result.returncode == 0, result.stderr
    [warning] = index_warnings(result)
    assert "catalog signature could not be fetched" in warning, warning
    assert "serves no catalog signature" not in warning, warning


def test_warn_names_the_size_cap_for_a_signature_over_it(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo, mode="warn")
    server.scripted_bodies[SIGNATURE_URL_PATH] = [b"x" * (300 * 1024)]

    result = update(ocx, repositories)

    assert result.returncode == 0, result.stderr
    [warning] = index_warnings(result)
    assert "262144-byte document cap" in warning, warning
    assert "HTTP status" not in warning, warning


def test_the_ocx_sh_default_warns_once_about_an_unsigned_catalog_and_succeeds(
    ocx: OcxRunner, unique_repo: str, server
) -> None:
    [repository] = [f"{unique_repo}/pkg"]
    write_index(server.root, [repository])
    configure_index(ocx, server)

    result = update(ocx, [repository])

    assert result.returncode == 0, result.stderr
    assert len(index_warnings(result)) == 1, result.stderr


def test_the_ocx_sh_default_warns_once_about_a_catalog_signed_by_an_unlisted_key(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    [repository] = [f"{unique_repo}/pkg"]
    write_index(server.root, [repository])
    sign_index(ocx, server.root, key=key_a)
    configure_index(ocx, server)

    result = update(ocx, [repository])

    assert result.returncode == 0, result.stderr
    assert len(index_warnings(result)) == 1, result.stderr


# ──────────────────────────────────────────────────────────────────────────────
# off, --no-verify, offline
# ──────────────────────────────────────────────────────────────────────────────


def test_a_registry_with_no_signers_makes_no_signature_request(
    ocx: OcxRunner, unique_repo: str, server
) -> None:
    [repository] = [f"{unique_repo}/pkg"]
    write_index(server.root, [repository], registry="idx.example")
    configure_index(ocx, server, registry="idx.example")

    result = update(ocx, [repository], registry="idx.example")

    assert result.returncode == 0, result.stderr
    assert server.count(SIGNATURE_URL_PATH) == 0, "mode off costs no extra request"
    assert index_warnings(result) == []


@pytest.mark.parametrize("how", ["flag", "env"])
def test_no_verify_skips_the_check_of_a_tampered_catalog_with_one_warning(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, key_b: Key, how: str
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo)
    break_signature("tampered", ocx, server, key_b)

    if how == "flag":
        result = update(ocx, repositories, "--no-verify")
    else:
        result = update(ocx, repositories, env={"OCX_NO_VERIFY": "1"})

    assert result.returncode == 0, result.stderr
    assert server.count(SIGNATURE_URL_PATH) == 0, (
        "a skipped check never fetches the signature"
    )
    skipped = [
        line for line in result.stderr.splitlines() if "no-verify" in line.lower()
    ]
    assert len(skipped) == 1, f"one warning line, got:\n{result.stderr}"


@pytest.mark.parametrize("verb", ["install", "pull"])
def test_package_no_verify_flag_skips_the_catalog_check_of_a_tampered_catalog(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    server,
    key_a: Key,
    key_b: Key,
    verb: str,
) -> None:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")
    repository = f"{unique_repo}/pkg"
    static_index.write_config(server.root)
    entry = static_index.write_package(
        server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf,
        os=os_name,
        architecture=arch_name,
    )
    static_index.write_catalog(server.root, {repository: entry.root_digest})
    sign_index(ocx, server.root, key=key_a)
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )
    break_signature("tampered", ocx, server, key_b)

    refused = ocx.run("package", verb, entry.logical_id, check=False)
    fetched = server.count(SIGNATURE_URL_PATH)
    skipped = ocx.run("package", verb, "--no-verify", entry.logical_id, check=False)

    assert_refused(refused, 65, "index_signature_invalid")
    assert skipped.returncode == 0, skipped.stderr
    assert server.count(SIGNATURE_URL_PATH) == fetched, (
        "the run with the flag fetches no signature"
    )


def test_a_committed_local_root_resolves_offline_with_zero_requests(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(ocx, server, key_a, unique_repo)
    assert update(ocx, repositories).returncode == 0
    before = len(server.requests)

    result = ocx.run(
        "--offline", "index", "list", f"ocx.sh/{repositories[0]}", check=False
    )

    assert result.returncode == 0, result.stderr
    assert len(server.requests) == before, "a committed root is never re-checked"


# ──────────────────────────────────────────────────────────────────────────────
# Every reader of a root checks its digest, committing or not
# ──────────────────────────────────────────────────────────────────────────────


def test_ocx_update_refuses_a_tampered_root_though_it_commits_nothing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key
) -> None:
    [repository] = serve(ocx, server, key_a, unique_repo)
    project = tmp_path / "project"
    project.mkdir()
    manifest = project / "ocx.toml"
    manifest.write_text(f'[tools]\npkg = "ocx.sh/{repository}:1.0.0"\n')
    root = server.root / "p" / f"{repository}.json"
    root.write_text(json.dumps(json.loads(root.read_text()), indent=1))

    result = ocx.run("--project", str(manifest), "update", check=False)

    assert_refused(result, 65, "index_root_digest_mismatch")


def test_package_prune_refuses_a_tampered_root(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key
) -> None:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False, index=False)
    leaf = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    repository = f"{unique_repo}/pkg"
    static_index.write_config(server.root)
    entry = static_index.write_package(
        server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf,
        ephemeral=True,
    )
    static_index.write_catalog(server.root, {repository: entry.root_digest})
    sign_index(ocx, server.root, key=key_a)
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )
    control = ocx.run(
        "package", "prune", "--dry-run", f"ocx.sh/{repository}", "1.0.0", check=False
    )
    assert "index_root_digest_mismatch" not in control.stdout, (
        "the untampered root passes the digest check"
    )

    root = server.root / "p" / f"{repository}.json"
    root.write_text(json.dumps(json.loads(root.read_text()), indent=1))
    result = ocx.run(
        "package", "prune", "--dry-run", f"ocx.sh/{repository}", "1.0.0", check=False
    )

    assert_refused(result, 65, "index_root_digest_mismatch")


# ──────────────────────────────────────────────────────────────────────────────
# Signers belong to one registry
# ──────────────────────────────────────────────────────────────────────────────


def test_a_catalog_signed_for_another_registry_is_invalid_under_a_shared_signer(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key
) -> None:
    repositories = serve(
        ocx,
        server,
        key_a,
        unique_repo,
        registry="idx-b.example",
        sign_registry="idx-a.example",
    )

    result = update(ocx, repositories, registry="idx-b.example")

    assert_refused(result, 65, "index_signature_invalid")
    output = result.stdout + result.stderr
    assert "idx-a.example" in output and "idx-b.example" in output, (
        "the refusal names the signed and the expected registry"
    )
    assert "predicate type mismatch" not in output, "the predicate type was right"


# ──────────────────────────────────────────────────────────────────────────────
# Managed configuration may only tighten, unless pinned
# ──────────────────────────────────────────────────────────────────────────────


def adopt_managed(ocx: OcxRunner, reference: str) -> None:
    result = ocx.run(
        "config", "update", check=False, env_overlay={"OCX_MANAGED_CONFIG": reference}
    )
    assert result.returncode == 0, f"managed-config adoption failed: {result.stderr}"


def test_an_unpinned_managed_source_cannot_replace_the_signer_list_but_a_pinned_one_can(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key, key_b: Key
) -> None:
    repositories = serve(ocx, server, key_b, unique_repo)
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )
    payload = f'[registries."ocx.sh"]\nindex_publishers = [{{ kind = "key", key_pem = {json.dumps(key_b.pem)} }}]\n'
    digest = push_managed_config(ocx, f"{unique_repo}_cfg", "v1", payload, tmp_path)

    floating = f"{ocx.registry}/{unique_repo}_cfg:v1"
    adopt_managed(ocx, floating)
    stripped = update(ocx, repositories, env={"OCX_MANAGED_CONFIG": floating})
    assert_refused(stripped, 65, "index_signature_invalid")
    assert (
        len(
            [
                line
                for line in stripped.stderr.splitlines()
                if "index_publishers" in line
            ]
        )
        == 1
    ), stripped.stderr

    pinned = f"{ocx.registry}/{unique_repo}_cfg@{digest}"
    adopt_managed(ocx, pinned)
    honoured = update(ocx, repositories, env={"OCX_MANAGED_CONFIG": pinned})
    assert honoured.returncode == 0, honoured.stderr


# ──────────────────────────────────────────────────────────────────────────────
# A release that predates the signature keeps installing
# ──────────────────────────────────────────────────────────────────────────────


def test_the_previous_release_reads_a_signed_catalog_with_an_expiry(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key
) -> None:
    old = previous_release.resolve()
    [repository] = serve(ocx, server, key_a, unique_repo)
    home = tmp_path / "old_home"
    home.mkdir()
    (home / "config.toml").write_text(
        f'[registries."ocx.sh"]\nindex = "{server.base_url}"\ntrusted_hosts = ["{ocx.registry.split(":", 1)[0]}"]\n'
    )
    env = {
        **ocx.env,
        "OCX_HOME": str(home),
        "OCX_INSECURE_REGISTRIES": f"{ocx.registry},{server.host}",
    }

    result = subprocess.run(
        [str(old), "index", "update", f"ocx.sh/{repository}"],
        capture_output=True,
        text=True,
        env=env,
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=120,
    )

    assert "expires" in (server.root / CATALOG).read_text(), (
        "the fixture must carry the new member"
    )
    assert result.returncode == 0, (
        f"previous release refused a signed catalog: {result.stderr}"
    )
