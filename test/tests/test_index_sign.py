# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx index sign <DIR> --registry <REGISTRY>`.

The command reads `<DIR>/c/index.json`, sets its `expires`, and writes the catalog
back beside a DSSE bundle `c/index.json.sigstore.json`. Signing happens before any
write, so a failed sign leaves both files as they were.
"""

from __future__ import annotations

import hashlib
import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from src import static_index
from src.index_signing import CATALOG, SIGNATURE, Key, make_key, sign_index, stack_table
from src.runner import OcxRunner
from tests.fixtures.sigstore_stack import SigstoreStack

pytestmark = pytest.mark.command("index_sign*")

REPORT_ROOT_KEYS = {
    "schema_version",
    "catalog_path",
    "signature_path",
    "catalog_digest",
    "expires_at",
    "signer",
}


@pytest.fixture()
def key(tmp_path: Path) -> Key:
    return make_key(tmp_path / "key")


@pytest.fixture()
def site(tmp_path: Path) -> Path:
    """A rendered index directory whose catalog carries two packages and no `expires`."""
    root = tmp_path / "site"
    root.mkdir()
    static_index.write_config(root)
    static_index.write_catalog(
        root, {"acme/tool": "sha256:" + "a" * 64, "acme/other": "sha256:" + "b" * 64}
    )
    return root


def parse_time(value: str) -> datetime:
    return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)


def assert_catalog_unchanged_but_for_expires(site: Path, original: dict) -> dict:
    signed = json.loads((site / CATALOG).read_text())
    expires = signed.pop("expires")
    assert signed == original, "every member other than `expires` is kept"
    return {"expires": expires}


def test_key_signing_writes_the_signature_and_sets_expires_thirty_days_out(
    ocx: OcxRunner, site: Path, key: Key
) -> None:
    original = json.loads((site / CATALOG).read_text())
    assert "expires" not in original

    result = sign_index(ocx, site, key=key)

    report = json.loads(result.stdout)
    assert REPORT_ROOT_KEYS <= report.keys()
    assert "tlog_index" not in report, (
        "a key signature uploaded to no log reports no tlog index"
    )
    assert (site / SIGNATURE).is_file()
    written = assert_catalog_unchanged_but_for_expires(site, original)["expires"]
    assert abs(
        parse_time(written) - (datetime.now(UTC) + timedelta(days=30))
    ) < timedelta(minutes=5)
    assert report["expires_at"] == written, (
        "the report names the expiry the catalog carries"
    )
    assert (
        report["catalog_digest"]
        == "sha256:" + hashlib.sha256((site / CATALOG).read_bytes()).hexdigest()
    )


def test_keyless_signing_against_the_local_stack_reports_the_signer_and_the_log_index(
    ocx: OcxRunner, site: Path, sigstore_stack: SigstoreStack, identity_token: Path
) -> None:
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(stack_table(sigstore_stack))

    result = sign_index(ocx, site, stack=sigstore_stack, token=identity_token)

    report = json.loads(result.stdout)
    assert REPORT_ROOT_KEYS <= report.keys()
    assert isinstance(report["tlog_index"], int), (
        "keyless signing records the statement in Rekor"
    )
    assert report["signer"] == "keyless-fulcio"
    assert report["certificate_identity"] == sigstore_stack.identity
    assert (site / SIGNATURE).is_file()


def test_expires_in_days_zero_writes_an_expiry_of_now(
    ocx: OcxRunner, site: Path, key: Key
) -> None:
    sign_index(ocx, site, key=key, expires_in_days=0)

    expires = parse_time(json.loads((site / CATALOG).read_text())["expires"])
    assert abs(expires - datetime.now(UTC)) < timedelta(minutes=5)


def test_signing_again_replaces_the_expiry_and_keeps_the_rest(
    ocx: OcxRunner, site: Path, key: Key
) -> None:
    original = json.loads((site / CATALOG).read_text())
    sign_index(ocx, site, key=key, expires_in_days=10)
    sign_index(ocx, site, key=key, expires_in_days=90)

    expires = parse_time(
        assert_catalog_unchanged_but_for_expires(site, original)["expires"]
    )
    assert abs(expires - (datetime.now(UTC) + timedelta(days=90))) < timedelta(
        minutes=5
    )


def test_a_failed_signature_leaves_both_files_untouched(
    ocx: OcxRunner, site: Path, tmp_path: Path
) -> None:
    before = (site / CATALOG).read_bytes()
    missing_key = tmp_path / "no-such.key"

    result = ocx.run(
        "index",
        "sign",
        str(site),
        "--registry",
        "ocx.sh",
        "--key",
        str(missing_key),
        check=False,
    )

    assert result.returncode != 0
    assert (site / CATALOG).read_bytes() == before
    assert not (site / SIGNATURE).exists()


def test_signing_is_refused_offline(ocx: OcxRunner, site: Path, key: Key) -> None:
    result = ocx.run(
        "--offline",
        "index",
        "sign",
        str(site),
        "--registry",
        "ocx.sh",
        "--key",
        str(key.private),
        check=False,
        env_overlay={"OCX_KEY_PASSWORD": "ocxtest-index"},
    )

    assert result.returncode == 81, result.stdout + result.stderr
    assert not (site / SIGNATURE).exists()


@pytest.mark.parametrize("flag", ["--signature-format", "--no-cache"])
def test_flags_the_command_does_not_have_are_unknown_arguments(
    ocx: OcxRunner, site: Path, key: Key, flag: str
) -> None:
    args = [
        "index",
        "sign",
        str(site),
        "--registry",
        "ocx.sh",
        "--key",
        str(key.private),
        flag,
    ]
    if flag == "--signature-format":
        args.append("cosign")

    result = ocx.run(*args, check=False)

    assert result.returncode == 64, result.stdout + result.stderr


def test_the_registry_flag_is_required(ocx: OcxRunner, site: Path, key: Key) -> None:
    result = ocx.run("index", "sign", str(site), "--key", str(key.private), check=False)

    assert result.returncode == 64, result.stdout + result.stderr


def test_an_expiry_beyond_the_maximum_is_a_usage_error(
    ocx: OcxRunner, site: Path, key: Key
) -> None:
    result = sign_index(ocx, site, key=key, expires_in_days=367, check=False)

    assert result.returncode == 64, result.stdout + result.stderr
    assert not (site / SIGNATURE).exists()


def test_a_catalog_that_does_not_parse_is_a_data_error_and_nothing_is_written(
    ocx: OcxRunner, site: Path, key: Key
) -> None:
    (site / CATALOG).write_text("{not json")

    result = sign_index(ocx, site, key=key, check=False)

    assert result.returncode == 65, result.stdout + result.stderr
    assert (site / CATALOG).read_text() == "{not json"
    assert not (site / SIGNATURE).exists()


def test_a_catalog_with_an_unsupported_format_version_is_refused_and_nothing_is_written(
    ocx: OcxRunner, site: Path, key: Key
) -> None:
    catalog = json.loads((site / CATALOG).read_text())
    catalog["format_version"] = 2
    (site / CATALOG).write_text(json.dumps(catalog))
    before = (site / CATALOG).read_bytes()

    result = sign_index(ocx, site, key=key, check=False)

    assert result.returncode == 65, result.stdout + result.stderr
    assert json.loads(result.stdout)["error"]["detail"] == "unsupported_index_format"
    assert (site / CATALOG).read_bytes() == before
    assert not (site / SIGNATURE).exists()


def test_unknown_catalog_members_survive_signing(
    ocx: OcxRunner, site: Path, key: Key
) -> None:
    document = json.loads((site / CATALOG).read_text())
    document.update(
        {"zeta": 1, "nested": {"a": {"b": [1, 2]}}, "alpha": "caf\u00e9"}
    )
    (site / CATALOG).write_text(json.dumps(document, indent=2) + "\n")

    sign_index(ocx, site, key=key)

    signed = json.loads((site / CATALOG).read_text())
    for member in ("zeta", "nested", "alpha"):
        assert signed[member] == document[member], f"`{member}` is kept as it was"


def test_the_plain_output_names_the_catalog_the_signature_and_the_expiry(
    ocx: OcxRunner, site: Path, key: Key
) -> None:
    result = ocx.run(
        "index",
        "sign",
        str(site),
        "--registry",
        "ocx.sh",
        "--key",
        str(key.private),
        format=None,
        env_overlay={"OCX_KEY_PASSWORD": "ocxtest-index"},
    )

    rows = [line.split(None, 1) for line in result.stdout.splitlines()]
    assert [row[0] for row in rows] == ["Item", "catalog", "signature", "expires"], (
        result.stdout
    )
    assert rows[1][1].strip() == str(site / CATALOG)
    assert rows[2][1].strip() == str(site / SIGNATURE)
    expires = json.loads((site / CATALOG).read_text())["expires"]
    assert rows[3][1].strip() == expires


@pytest.mark.parametrize(
    "registry",
    ["OCX.SH", "ocx.sh:443", "ocx.sh/index", "xn--é.com", "https://ocx.sh", "", "ocx.sh.", "ocx.sh..", "localhost.", "ocx.sh:0"],
)
def test_a_registry_not_spelled_as_clients_compare_it_is_a_usage_error(
    ocx: OcxRunner, site: Path, key: Key, registry: str
) -> None:
    before = (site / CATALOG).read_bytes()

    result = sign_index(ocx, site, key=key, registry=registry, check=False)

    assert result.returncode == 64, result.stdout + result.stderr
    assert (site / CATALOG).read_bytes() == before
    assert not (site / SIGNATURE).exists()
