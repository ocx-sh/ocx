# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the signed index catalog check posture.

One decision covers a run: `--verify` / `--no-verify` outrank `OCX_NO_VERIFY`, which outranks the
check the registry's `index_signature` asks for. A registry-role `[mirrors]` entry that hides the
index of an `enforce` namespace refuses that registry's first index read, and a `[registries]` key no
client spells is warned about.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import static_index
from src.helpers import make_package
from src.index_signing import (
    SIGNATURE_URL_PATH,
    Key,
    assert_refused,
    configure_index,
    make_key,
    sign_index,
    write_index,
)
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner

pytestmark = pytest.mark.command(
    "index_update*", "index_catalog", "index_list", "install"
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


def tampered_index(
    ocx: OcxRunner,
    server: static_index.StaticIndexServer,
    key_a: Key,
    key_b: Key,
    repository: str,
) -> None:
    """Serve `repository` under `enforce`, with a catalog whose signature is not from the listed key."""
    write_index(server.root, [repository])
    sign_index(ocx, server.root, key=key_a)
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )
    sign_index(ocx, server.root, key=key_b)


def installable_tampered_index(
    ocx: OcxRunner,
    server: static_index.StaticIndexServer,
    key_a: Key,
    key_b: Key,
    unique_repo: str,
    tmp_path: Path,
) -> str:
    """Serve a real package under `enforce` with a forged catalog signature; returns its logical id."""
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
    sign_index(ocx, server.root, key=key_b)
    return entry.logical_id


# ──────────────────────────────────────────────────────────────────────────────
# --verify outranks OCX_NO_VERIFY
# ──────────────────────────────────────────────────────────────────────────────


def test_index_update_verify_checks_the_catalog_though_the_env_opts_out(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, key_b: Key
) -> None:
    repository = f"{unique_repo}/pkg0"
    tampered_index(ocx, server, key_a, key_b, repository)

    result = ocx.run(
        "index",
        "update",
        "--verify",
        f"ocx.sh/{repository}",
        check=False,
        env_overlay={"OCX_NO_VERIFY": "1"},
    )

    assert_refused(result, 65, "index_signature_invalid")


def test_index_sync_verify_checks_the_catalog_though_the_env_opts_out(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, key_b: Key
) -> None:
    tampered_index(ocx, server, key_a, key_b, f"{unique_repo}/pkg0")

    result = ocx.run(
        "index",
        "sync",
        "--verify",
        "ocx.sh",
        check=False,
        env_overlay={"OCX_NO_VERIFY": "1"},
    )

    assert_refused(result, 65, "index_signature_invalid")


def test_index_catalog_remote_verify_checks_the_catalog_though_the_env_opts_out(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, key_b: Key
) -> None:
    tampered_index(ocx, server, key_a, key_b, f"{unique_repo}/pkg0")

    result = ocx.run(
        "--remote",
        "index",
        "catalog",
        "--verify",
        "ocx.sh",
        check=False,
        env_overlay={"OCX_NO_VERIFY": "1"},
    )

    assert_refused(result, 65, "index_signature_invalid")


def test_index_catalog_remote_no_verify_skips_the_check_with_one_warning(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, key_b: Key
) -> None:
    repository = f"{unique_repo}/pkg0"
    tampered_index(ocx, server, key_a, key_b, repository)

    result = ocx.run(
        "--remote", "index", "catalog", "--no-verify", "ocx.sh", check=False
    )

    assert result.returncode == 0, result.stderr
    assert server.count(SIGNATURE_URL_PATH) == 0
    assert repository in result.stdout
    skipped = [
        line for line in result.stderr.splitlines() if "no-verify" in line.lower()
    ]
    assert len(skipped) == 1, f"one warning line, got:\n{result.stderr}"


def test_index_list_remote_verify_checks_the_catalog_though_the_env_opts_out(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, key_b: Key
) -> None:
    repository = f"{unique_repo}/pkg0"
    tampered_index(ocx, server, key_a, key_b, repository)

    result = ocx.run(
        "--remote",
        "index",
        "list",
        "--verify",
        f"ocx.sh/{repository}",
        check=False,
        env_overlay={"OCX_NO_VERIFY": "1"},
    )

    assert_refused(result, 65, "index_signature_invalid")


def test_package_install_verify_checks_the_catalog_though_the_env_opts_out(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key, key_b: Key
) -> None:
    logical_id = installable_tampered_index(
        ocx, server, key_a, key_b, unique_repo, tmp_path
    )

    result = ocx.run(
        "package",
        "install",
        "--verify",
        logical_id,
        check=False,
        env_overlay={"OCX_NO_VERIFY": "1"},
    )

    assert_refused(result, 65, "index_signature_invalid")


def test_the_env_opt_out_alone_still_skips_the_check_with_one_warning(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, key_b: Key
) -> None:
    repository = f"{unique_repo}/pkg0"
    tampered_index(ocx, server, key_a, key_b, repository)

    result = ocx.run(
        "index",
        "update",
        f"ocx.sh/{repository}",
        check=False,
        env_overlay={"OCX_NO_VERIFY": "1"},
    )

    assert result.returncode == 0, result.stderr
    assert server.count(SIGNATURE_URL_PATH) == 0
    skipped = [
        line for line in result.stderr.splitlines() if "no-verify" in line.lower()
    ]
    assert len(skipped) == 1, f"one warning line, got:\n{result.stderr}"


def test_package_install_with_the_env_opt_out_alone_skips_the_check(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key, key_b: Key
) -> None:
    logical_id = installable_tampered_index(
        ocx, server, key_a, key_b, unique_repo, tmp_path
    )

    result = ocx.run(
        "package",
        "install",
        logical_id,
        check=False,
        env_overlay={"OCX_NO_VERIFY": "1"},
    )

    assert result.returncode == 0, result.stderr
    assert server.count(SIGNATURE_URL_PATH) == 0


# ──────────────────────────────────────────────────────────────────────────────
# a registry mirror that hides the index of an enforce namespace
# ──────────────────────────────────────────────────────────────────────────────


def write_mirrored_config(ocx: OcxRunner, mode: str | None) -> None:
    lines = [f'[mirrors."ocx.sh"]\nregistry = "http://{ocx.registry}"\n']
    if mode is not None:
        lines.append(f'[registries."ocx.sh"]\nindex_signature = "{mode}"\n')
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text("\n".join(lines))


def test_enforce_refuses_a_registry_mirror_that_hides_the_index(
    ocx: OcxRunner, unique_repo: str
) -> None:
    write_mirrored_config(ocx, "enforce")

    result = ocx.run(
        "--remote", "index", "list", f"ocx.sh/{unique_repo}/pkg", check=False
    )

    assert_refused(result, 78, "index_catalog_through_mirror")
    message = json.loads(result.stdout)["error"]["message"]
    assert '[mirrors."ocx.sh"]' in message, message
    assert "signed index catalog cannot be checked through a registry mirror" in message


def test_enforce_through_a_registry_mirror_refuses_index_sync(ocx: OcxRunner) -> None:
    write_mirrored_config(ocx, "enforce")

    result = ocx.run("index", "sync", "ocx.sh", check=False)

    assert_refused(result, 78, "index_catalog_through_mirror")


def test_enforce_through_a_registry_mirror_refuses_index_update_of_a_named_package(
    ocx: OcxRunner, unique_repo: str
) -> None:
    write_mirrored_config(ocx, "enforce")

    result = ocx.run("index", "update", f"ocx.sh/{unique_repo}/pkg", check=False)

    assert_refused(result, 78, "index_catalog_through_mirror")


def test_enforce_through_a_registry_mirror_refuses_a_remote_catalog_listing(
    ocx: OcxRunner,
) -> None:
    write_mirrored_config(ocx, "enforce")

    result = ocx.run("--remote", "index", "catalog", "ocx.sh", check=False)

    assert_refused(result, 78, "index_catalog_through_mirror")


@pytest.mark.parametrize(
    "argv",
    [
        ("clean", "--force"),
        ("self", "activate"),
        ("--project", "{project}", "exec", "--", "true"),
        ("--offline", "--project", "{project}", "exec", "--", "true"),
    ],
    ids=["clean", "self-activate", "exec", "offline-exec"],
)
def test_enforce_through_a_registry_mirror_leaves_a_command_that_reads_no_index_alone(
    ocx: OcxRunner, tmp_path: Path, argv: tuple[str, ...]
) -> None:
    write_mirrored_config(ocx, "enforce")
    # An empty project, so `exec` has nothing to resolve and exits on its own merits.
    project = tmp_path / "empty_project" / "ocx.toml"
    project.parent.mkdir()
    project.write_text("[tools]\n")
    ocx.run("--project", str(project), "lock", "--no-pull")

    result = ocx.run(*(arg.format(project=project) for arg in argv), check=False)

    assert result.returncode == 0, (result.stdout, result.stderr)
    assert "index_catalog_through_mirror" not in result.stdout, result.stdout


@pytest.mark.parametrize(
    ("flags", "env"),
    [(["--no-verify"], {}), ([], {"OCX_NO_VERIFY": "1"})],
    ids=["flag", "env"],
)
def test_the_verify_opt_out_clears_the_registry_mirror_refusal(
    ocx: OcxRunner, flags: list[str], env: dict[str, str]
) -> None:
    write_mirrored_config(ocx, "enforce")

    result = ocx.run(
        "--remote",
        "index",
        "catalog",
        "ocx.sh",
        *flags,
        check=False,
        env_overlay=env,
    )

    assert result.returncode == 0, (result.stdout, result.stderr)
    assert "index_catalog_through_mirror" not in result.stdout, result.stdout
    assert "no-verify" in result.stderr.lower(), result.stderr


def test_enforce_through_a_registry_mirror_does_not_refuse_offline(
    ocx: OcxRunner, unique_repo: str
) -> None:
    write_mirrored_config(ocx, "enforce")

    result = ocx.run(
        "--offline", "index", "list", f"ocx.sh/{unique_repo}/pkg", check=False
    )

    assert result.returncode == 0, (result.stdout, result.stderr)
    assert "index_catalog_through_mirror" not in result.stdout


def test_warn_through_a_registry_mirror_says_the_catalog_is_not_checked(
    ocx: OcxRunner, unique_repo: str
) -> None:
    write_mirrored_config(ocx, None)

    result = ocx.run("index", "list", f"ocx.sh/{unique_repo}/pkg", check=False)

    assert "index catalog signature is not checked" in result.stderr, result.stderr


# ──────────────────────────────────────────────────────────────────────────────
# a [registries] key no client spells
# ──────────────────────────────────────────────────────────────────────────────


def test_an_uppercase_registry_key_warns_naming_both_spellings(
    ocx: OcxRunner, unique_repo: str
) -> None:
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        '[registries."OCX.SH"]\nindex_signature = "enforce"\n'
    )

    result = ocx.run(
        "--offline", "index", "list", f"ocx.sh/{unique_repo}/pkg", check=False
    )

    [warning] = [line for line in result.stderr.splitlines() if "never applied" in line]
    assert '[registries."OCX.SH"]' in warning, warning
    assert '"ocx.sh"' in warning, warning


# ──────────────────────────────────────────────────────────────────────────────
# Recovery after switching to enforce
# ──────────────────────────────────────────────────────────────────────────────


def test_index_update_after_switching_to_enforce_rereads_a_root_committed_under_warn(
    ocx: OcxRunner, unique_repo: str, server, key_a: Key, key_b: Key
) -> None:
    repository = f"{unique_repo}/pkg0"
    write_index(server.root, [repository])
    sign_index(ocx, server.root, key=key_a)
    sign_index(
        ocx, server.root, key=key_b
    )  # the served signature is not from the listed key
    configure_index(ocx, server, index_signature="warn", index_publishers=[key_a.entry])

    committed = ocx.run("index", "update", f"ocx.sh/{repository}", check=False)
    assert committed.returncode == 0, committed.stderr

    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )
    refused = ocx.run("index", "update", f"ocx.sh/{repository}", check=False)

    assert_refused(refused, 65, "index_signature_invalid")
