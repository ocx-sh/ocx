# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the `[sigstore."<authority>"]` instance tables.

A Sigstore instance (Fulcio, Rekor, trust root) is named by the host[:port] of its
Fulcio URL. A keyless signer entry picks its instance with `sigstore = "<authority>"`
and falls back to the public one. The retired `[trust.sigstore]` table fails
closed, because ignoring it would widen trust to the public instance.

The private stack of the suite (`localhost:5555`) is the non-public instance; a
foreign trust root stands in for the public one so no test reaches the internet.
"""

from __future__ import annotations

import base64
import datetime as dt
import json
import subprocess
from collections.abc import Iterator
from pathlib import Path

import pytest
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import NameOID

from src import static_index
from src.helpers import push_managed_config, rglob_no_links
from src.index_signing import (
    CATALOG,
    Key,
    assert_refused,
    authority,
    configure_index,
    index_warnings,
    keyless_entry,
    make_key,
    sign_index,
    stack_table,
    write_index,
)
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures.sigstore_stack import SigstoreStack

pytestmark = pytest.mark.command(
    "config*", "package_sign*", "package_verify*", "install", "index_update*"
)

#: Any outbound HTTPS goes to a closed port, so a run that needs a Sigstore service fails instead of passing quietly.
NO_NETWORK = {"HTTPS_PROXY": "http://127.0.0.1:9", "https_proxy": "http://127.0.0.1:9"}

RETIRED_FORMS = {
    "table": '[trust.sigstore]\nrekor_url = "https://rekor.example"\n',
    "dotted_key": 'trust.sigstore.rekor_url = "https://rekor.example"\n',
    "inline_table": 'trust = { sigstore = { rekor_url = "https://rekor.example" } }\n',
    "array_of_tables": '[[trust.sigstore]]\nrekor_url = "https://rekor.example"\n',
}


def config_path(ocx: OcxRunner) -> Path:
    return Path(ocx.env["OCX_HOME"]) / "config.toml"


def foreign_trusted_root(directory: Path, stack: SigstoreStack) -> Path:
    """The stack's trust root with its CA swapped for an unrelated one: a private certificate cannot chain to it."""
    document = json.loads(stack.trusted_root_json.read_text())
    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "unrelated CA")])
    now = dt.datetime.now(dt.UTC)
    certificate = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - dt.timedelta(days=1))
        .not_valid_after(now + dt.timedelta(days=30))
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .sign(key, hashes.SHA256())
    )
    raw = base64.b64encode(
        certificate.public_bytes(serialization.Encoding.DER)
    ).decode()
    for authority_entry in document["certificateAuthorities"]:
        authority_entry["certChain"]["certificates"] = [{"rawBytes": raw}]
    target = directory / "foreign_trusted_root.json"
    target.write_text(json.dumps(document))
    return target


def seed_public_root(ocx: OcxRunner, root: Path) -> None:
    """Put `root` where the public instance reads its trust root, so no TUF fetch is needed."""
    convention = Path(ocx.env["OCX_HOME"]) / "sigstore" / "trusted-root.json"
    convention.parent.mkdir(parents=True, exist_ok=True)
    convention.write_bytes(root.read_bytes())


def sign_privately(
    ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo
) -> None:
    result = ocx.run("package", "sign", *stack.sign_args(token), pkg.short, check=False)
    assert result.returncode == 0, f"sign setup failed: {result.stderr}"


def write_policy(
    ocx: OcxRunner,
    pkg: PackageInfo,
    stack: SigstoreStack,
    *,
    instance: str | None,
    tables: str,
) -> None:
    signer = f'kind = "keyless", identity = "{stack.identity}", oidc_issuer = "{stack.issuer}"'
    if instance is not None:
        signer += f', sigstore = "{instance}"'
    config_path(ocx).write_text(
        f'[[trust.policy]]\nscope = "{ocx.registry}/{pkg.repo}"\nsigners = [{{ {signer} }}]\n{tables}'
    )


def install(
    ocx: OcxRunner, pkg: PackageInfo, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return ocx.run(
        "package",
        "install",
        "-p",
        current_platform(),
        pkg.short,
        check=False,
        env_overlay=env,
    )


def assert_nothing_installed(ocx: OcxRunner) -> None:
    packages = ocx.ocx_home / "packages"
    assert not packages.exists() or not list(rglob_no_links(packages, "metadata.json"))


# ──────────────────────────────────────────────────────────────────────────────
# The retired table fails closed
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("form", RETIRED_FORMS)
def test_a_config_with_the_retired_trust_sigstore_table_fails_every_command_with_78(
    ocx: OcxRunner, form: str
) -> None:
    config_path(ocx).write_text(RETIRED_FORMS[form])

    result = ocx.run("--offline", "index", "list", "ocx.sh/anything", check=False)

    assert_refused(result, 78, "trust_sigstore_retired")
    assert "config.toml" in result.stderr + result.stdout, "the message names the file"
    assert '[sigstore."' in result.stderr + result.stdout, (
        "the message names the table that replaces it"
    )


def test_config_test_names_the_file_that_declares_the_retired_table(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    candidate = tmp_path / "candidate.toml"
    candidate.write_text(RETIRED_FORMS["table"])

    result = ocx.run("config", "test", str(candidate), check=False)

    assert_refused(result, 78, "trust_sigstore_retired")
    assert str(candidate) in result.stderr, result.stderr


@pytest.mark.parametrize(
    ("payload", "slug"),
    [
        (
            '[sigstore."localhost:5555"]\ntrusted_root = "root.json"\ntrusted_root_json = "{}"\n',
            "ambiguous_trust_root",
        ),
        (
            (
                '[sigstore."localhost:5555"]\nrekor_url = "https://a.example"\n'
                '[sigstore."LOCALHOST:5555"]\nrekor_url = "https://b.example"\n'
            ),
            "sigstore_authority_invalid",
        ),
        ('[sigstore."a b"]\nrekor_url = "https://a.example"\n', "sigstore_authority_invalid"),
    ],
)
def test_config_test_names_the_file_for_every_sigstore_table_refusal(
    ocx: OcxRunner, tmp_path: Path, payload: str, slug: str
) -> None:
    candidate = tmp_path / "candidate.toml"
    candidate.write_text(payload)

    result = ocx.run("config", "test", str(candidate), check=False)

    assert_refused(result, 78, slug)
    assert str(candidate) in result.stderr, result.stderr


def test_config_push_names_the_table_declaring_both_trust_root_spellings(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    payload = tmp_path / "config.toml"
    payload.write_text(
        '[sigstore."localhost:5555"]\ntrusted_root = "root.json"\ntrusted_root_json = "{}"\n'
    )

    result = ocx.run(
        "config", "push", "-i", "127.0.0.1:9/never/reached:v1", str(payload), check=False
    )

    assert_refused(result, 78, "ambiguous_trust_root")
    assert "localhost:5555" in result.stderr, result.stderr


def test_config_push_refuses_a_payload_with_the_retired_table_before_any_network(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    payload = tmp_path / "config.toml"
    payload.write_text(RETIRED_FORMS["table"])

    result = ocx.run(
        "config",
        "push",
        "-i",
        "127.0.0.1:9/never/reached:v1",
        str(payload),
        check=False,
    )

    assert_refused(result, 78, "trust_sigstore_retired")


def test_a_stale_managed_snapshot_with_the_retired_table_fails_commands_but_config_update_refreshes_it(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    payload = '[registries."snapshot-marker.example"]\ninsecure = true\n'
    digest = push_managed_config(ocx, f"{unique_repo}_cfg", "v1", payload, tmp_path)
    pinned = f"{ocx.registry}/{unique_repo}_cfg@{digest}"
    env = {"OCX_MANAGED_CONFIG": pinned}
    assert ocx.run("config", "update", check=False, env_overlay=env).returncode == 0
    snapshots = [
        path
        for path in rglob_no_links(Path(ocx.env["OCX_HOME"]), "*")
        if path.is_file()
        and "snapshot-marker.example" in path.read_text(errors="ignore")
    ]
    assert snapshots, "the adopted snapshot must exist on disk"
    for snapshot in snapshots:
        snapshot.write_text(snapshot.read_text() + "\n" + RETIRED_FORMS["table"])

    refused = ocx.run(
        "--offline", "index", "list", "ocx.sh/anything", check=False, env_overlay=env
    )
    assert_refused(refused, 78, "trust_sigstore_retired")

    refreshed = ocx.run("config", "update", check=False, env_overlay=env)
    assert refreshed.returncode == 0, refreshed.stderr
    assert "trust.sigstore" in refreshed.stderr, (
        "config update warns about the stale table"
    )
    healed = ocx.run(
        "--offline", "index", "list", "ocx.sh/anything", check=False, env_overlay=env
    )
    assert healed.returncode != 78, (
        "the refreshed snapshot no longer carries the retired table"
    )


# ──────────────────────────────────────────────────────────────────────────────
# A signer entry selects its instance
# ──────────────────────────────────────────────────────────────────────────────


def test_a_keyless_signer_naming_the_private_instance_verifies_with_its_table(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    config_path(ocx).write_text(stack_table(sigstore_stack))
    sign_privately(ocx, sigstore_stack, identity_token, published_package)
    write_policy(
        ocx,
        published_package,
        sigstore_stack,
        instance=authority(sigstore_stack),
        tables=stack_table(sigstore_stack),
    )

    result = install(ocx, published_package)

    assert result.returncode == 0, result.stderr


def test_the_same_signature_against_a_signer_without_the_field_is_a_mismatch_with_the_public_root(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    config_path(ocx).write_text(stack_table(sigstore_stack))
    sign_privately(ocx, sigstore_stack, identity_token, published_package)
    write_policy(ocx, published_package, sigstore_stack, instance=None, tables="")
    seed_public_root(ocx, foreign_trusted_root(tmp_path, sigstore_stack))

    result = install(ocx, published_package)

    assert result.returncode == 77, result.stderr
    assert_nothing_installed(ocx)


def test_a_signer_naming_an_instance_with_no_table_is_a_mismatch_and_reaches_no_network(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    config_path(ocx).write_text(stack_table(sigstore_stack))
    sign_privately(ocx, sigstore_stack, identity_token, published_package)
    write_policy(
        ocx, published_package, sigstore_stack, instance="fulcio.other", tables=""
    )

    result = install(ocx, published_package, NO_NETWORK)

    assert result.returncode == 77, result.stderr
    assert_nothing_installed(ocx)


def test_the_trusted_root_variable_does_not_replace_the_public_instances_root_during_auto_verify(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Install-time verification ignores the variable: the seeded (foreign) public root decides."""
    config_path(ocx).write_text(stack_table(sigstore_stack))
    sign_privately(ocx, sigstore_stack, identity_token, published_package)
    write_policy(ocx, published_package, sigstore_stack, instance=None, tables="")
    seed_public_root(ocx, foreign_trusted_root(tmp_path, sigstore_stack))
    private_root = {"OCX_SIGSTORE_TRUSTED_ROOT": str(sigstore_stack.trusted_root_json)}

    result = install(ocx, published_package, private_root)

    assert result.returncode == 77, (
        "the variable must not replace the seeded public root: " + result.stderr
    )
    assert_nothing_installed(ocx)


def test_the_trusted_root_variable_does_not_replace_the_public_instances_root_in_the_catalog_check(
    ocx: OcxRunner,
    unique_repo: str,
    server,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """The catalog check ignores the variable: the seeded public root decides, against the variable both ways."""
    repository = f"{unique_repo}/pkg"
    write_index(server.root, [repository])
    publishers = [keyless_entry(sigstore_stack, instance=False)]
    configure_index(
        ocx,
        server,
        index_signature="enforce",
        index_publishers=publishers,
        extra_toml=stack_table(sigstore_stack),
    )
    sign_index(ocx, server.root, stack=sigstore_stack, token=identity_token)
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=publishers
    )
    foreign_root = foreign_trusted_root(tmp_path, sigstore_stack)

    def update(variable: Path) -> subprocess.CompletedProcess[str]:
        overlay = {"OCX_SIGSTORE_TRUSTED_ROOT": str(variable)}
        return ocx.run(
            "index", "update", f"ocx.sh/{repository}", check=False, env_overlay=overlay
        )

    seed_public_root(ocx, foreign_root)
    refused = update(sigstore_stack.trusted_root_json)
    assert_refused(refused, 65, "index_signature_invalid")

    seed_public_root(ocx, sigstore_stack.trusted_root_json)
    accepted = update(foreign_root)
    assert accepted.returncode == 0, accepted.stderr


def test_the_trusted_root_variable_never_replaces_a_private_instances_own_root_during_auto_verify(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    config_path(ocx).write_text(stack_table(sigstore_stack))
    sign_privately(ocx, sigstore_stack, identity_token, published_package)
    write_policy(
        ocx,
        published_package,
        sigstore_stack,
        instance=authority(sigstore_stack),
        tables=stack_table(sigstore_stack),
    )
    foreign = {
        "OCX_SIGSTORE_TRUSTED_ROOT": str(foreign_trusted_root(tmp_path, sigstore_stack))
    }

    result = install(ocx, published_package, foreign)

    assert result.returncode == 0, result.stderr


def test_the_trusted_root_variable_never_supplies_a_private_instances_root_during_auto_verify(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    config_path(ocx).write_text(stack_table(sigstore_stack))
    sign_privately(ocx, sigstore_stack, identity_token, published_package)
    bare_table = f'[sigstore."{authority(sigstore_stack)}"]\nfulcio_url = "{sigstore_stack.fulcio_url}"\n'
    write_policy(
        ocx,
        published_package,
        sigstore_stack,
        instance=authority(sigstore_stack),
        tables=bare_table,
    )
    private_root = {"OCX_SIGSTORE_TRUSTED_ROOT": str(sigstore_stack.trusted_root_json)}

    result = install(ocx, published_package, private_root)

    assert_refused(result, 78, "sigstore_trusted_root_missing")


def verify(
    ocx: OcxRunner, pkg: PackageInfo, env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    return ocx.run(
        "package", "verify", "-p", current_platform(), pkg.short, check=False, env_overlay=env
    )


def test_the_trusted_root_variable_never_replaces_a_private_instances_own_root_in_policy_mode_verify(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """The variable reaches the selected instance only; a private instance keeps the root of its table."""
    config_path(ocx).write_text(stack_table(sigstore_stack))
    sign_privately(ocx, sigstore_stack, identity_token, published_package)
    write_policy(
        ocx,
        published_package,
        sigstore_stack,
        instance=authority(sigstore_stack),
        tables=stack_table(sigstore_stack),
    )
    foreign = {
        **NO_NETWORK,
        "OCX_SIGSTORE_TRUSTED_ROOT": str(foreign_trusted_root(tmp_path, sigstore_stack)),
    }

    result = verify(ocx, published_package, foreign)

    assert result.returncode == 0, result.stderr


def test_the_trusted_root_variable_never_supplies_a_private_instances_root_in_policy_mode_verify(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    config_path(ocx).write_text(stack_table(sigstore_stack))
    sign_privately(ocx, sigstore_stack, identity_token, published_package)
    bare_table = f'[sigstore."{authority(sigstore_stack)}"]\nfulcio_url = "{sigstore_stack.fulcio_url}"\n'
    write_policy(
        ocx,
        published_package,
        sigstore_stack,
        instance=authority(sigstore_stack),
        tables=bare_table,
    )
    private_root = {
        **NO_NETWORK,
        "OCX_SIGSTORE_TRUSTED_ROOT": str(sigstore_stack.trusted_root_json),
    }

    result = verify(ocx, published_package, private_root)

    assert_refused(result, 78, "sigstore_trusted_root_missing")


def test_a_table_declaring_both_trust_root_spellings_is_refused_at_load(
    ocx: OcxRunner, sigstore_stack: SigstoreStack
) -> None:
    config_path(ocx).write_text(
        stack_table(sigstore_stack) + 'trusted_root_json = "{}"\n'
    )

    result = ocx.run("--offline", "index", "list", "ocx.sh/anything", check=False)

    assert_refused(result, 78, "ambiguous_trust_root")


def test_two_table_keys_spelling_one_authority_are_refused_at_load(ocx: OcxRunner) -> None:
    config_path(ocx).write_text(
        '[sigstore."localhost:5555"]\nrekor_url = "https://a.example"\n'
        '[sigstore."LOCALHOST:5555"]\nrekor_url = "https://b.example"\n'
    )

    result = ocx.run("--offline", "index", "list", "ocx.sh/anything", check=False)

    assert_refused(result, 78, "sigstore_authority_invalid")
    assert "LOCALHOST:5555" in result.stderr and "localhost:5555" in result.stderr


# ──────────────────────────────────────────────────────────────────────────────
# Signing selects the table from the Fulcio URL
# ──────────────────────────────────────────────────────────────────────────────


def test_signing_takes_the_rekor_endpoint_and_trust_root_from_the_table_of_the_fulcio_url(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    config_path(ocx).write_text(stack_table(sigstore_stack))

    result = ocx.run(
        "package",
        "sign",
        "--platform",
        current_platform(),
        "--fulcio-url",
        sigstore_stack.fulcio_url,
        "--identity-token-file",
        str(identity_token),
        published_package.short,
        check=False,
    )

    assert result.returncode == 0, (
        f"no --rekor-url: the table supplies it\n{result.stderr}"
    )
    verified = ocx.run(
        "package",
        "verify",
        *sigstore_stack.verify_args(),
        "--sigstore",
        authority(sigstore_stack),
        published_package.short,
        check=False,
    )
    assert verified.returncode == 0, verified.stderr


def test_a_table_key_that_differs_from_its_fulcio_url_authority_is_a_config_error(
    ocx: OcxRunner,
) -> None:
    config_path(ocx).write_text(
        '[sigstore."localhost:5555"]\nfulcio_url = "http://localhost:6666"\n'
    )

    result = ocx.run("--offline", "index", "list", "ocx.sh/anything", check=False)

    assert_refused(result, 78, "sigstore_table_key_mismatch")


@pytest.mark.parametrize(
    "key", ["https://fulcio.example", "fulcio.example/path", "fulcio.example."]
)
def test_a_table_key_that_is_not_a_bare_authority_is_a_config_error(
    ocx: OcxRunner, key: str
) -> None:
    config_path(ocx).write_text(
        f'[sigstore."{key}"]\nrekor_url = "https://rekor.example"\n'
    )

    result = ocx.run("--offline", "index", "list", "ocx.sh/anything", check=False)

    assert_refused(result, 78, "sigstore_authority_invalid")


def test_signing_with_a_rekor_upload_against_a_table_without_a_rekor_url_is_a_config_error(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    config_path(ocx).write_text(
        f'[sigstore."{authority(sigstore_stack)}"]\nfulcio_url = "{sigstore_stack.fulcio_url}"\n'
    )

    result = ocx.run(
        "package",
        "sign",
        "--platform",
        current_platform(),
        "--fulcio-url",
        sigstore_stack.fulcio_url,
        "--identity-token-file",
        str(identity_token),
        published_package.short,
        check=False,
    )

    assert_refused(result, 78, "sigstore_rekor_url_missing")


# ──────────────────────────────────────────────────────────────────────────────
# Managed configuration: an unpinned source may not loosen or redirect
# ──────────────────────────────────────────────────────────────────────────────


@pytest.fixture()
def key_a(tmp_path: Path) -> Key:
    return make_key(tmp_path / "key_a")


@pytest.fixture()
def server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as running:
        yield running


def adopt(ocx: OcxRunner, reference: str) -> dict[str, str]:
    env = {"OCX_MANAGED_CONFIG": reference}
    result = ocx.run("config", "update", check=False, env_overlay=env)
    assert result.returncode == 0, f"managed-config adoption failed: {result.stderr}"
    return env


def test_an_unpinned_managed_source_cannot_switch_index_checking_off_but_a_pinned_one_can(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, server, key_a: Key
) -> None:
    [repository] = [f"{unique_repo}/pkg"]
    write_index(server.root, [repository])
    sign_index(ocx, server.root, key=key_a)
    catalog = server.root / CATALOG
    catalog.write_bytes(catalog.read_bytes() + b"\n")
    configure_index(
        ocx, server, index_signature="enforce", index_publishers=[key_a.entry]
    )
    digest = push_managed_config(
        ocx,
        f"{unique_repo}_cfg",
        "v1",
        '[registries."ocx.sh"]\nindex_signature = "off"\n',
        tmp_path,
    )

    floating = adopt(ocx, f"{ocx.registry}/{unique_repo}_cfg:v1")
    dropped = ocx.run(
        "index", "update", f"ocx.sh/{repository}", check=False, env_overlay=floating
    )
    assert_refused(dropped, 65, "index_signature_invalid")
    assert (
        len(
            [
                line
                for line in dropped.stderr.splitlines()
                if "index_signature" in line and "ocx.sh" in line
            ]
        )
        == 1
    )

    pinned = adopt(ocx, f"{ocx.registry}/{unique_repo}_cfg@{digest}")
    honoured = ocx.run(
        "index", "update", f"ocx.sh/{repository}", check=False, env_overlay=pinned
    )
    assert honoured.returncode == 0, honoured.stderr
    assert index_warnings(honoured) == []


def test_an_unpinned_managed_source_cannot_turn_the_transparency_log_upload_off(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    payload = '[sigstore."localhost:5555"]\nrekor_upload = false\n'
    digest = push_managed_config(ocx, f"{unique_repo}_cfg", "v1", payload, tmp_path)

    def probe(reference: str) -> subprocess.CompletedProcess[str]:
        """Adopt `reference`, then run an ordinary command: the guard warns when the snapshot is loaded."""
        env = adopt(ocx, reference)
        return ocx.run(
            "--offline", "index", "list", "ocx.sh/anything", check=False, env_overlay=env
        )

    floating = probe(f"{ocx.registry}/{unique_repo}_cfg:v1")
    pinned = probe(f"{ocx.registry}/{unique_repo}_cfg@{digest}")

    assert (
        len([line for line in floating.stderr.splitlines() if "rekor_upload" in line])
        == 1
    ), floating.stderr
    assert [
        line for line in pinned.stderr.splitlines() if "rekor_upload" in line
    ] == [], pinned.stderr
