"""Helpers for specs that sign an index with `ocx index sign` and point a registry at it.

Kept apart from `static_index` (wire shapes and the HTTP fixture) because everything
here drives the `ocx` binary: keys, the Sigstore-instance table of the local stack,
the signing command and the `config.toml` that names the signers.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import subprocess
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING
from urllib.parse import urlparse

import pytest

from src import static_index
from src.helpers import SIGSTORE_DIR

if TYPE_CHECKING:
    from src.runner import OcxRunner
    from tests.fixtures.sigstore_stack import SigstoreStack

KEY_PASSWORD = "ocxtest-index"
CATALOG = "c/index.json"
SIGNATURE = "c/index.json.sigstore.json"
#: What a client requests: the signature path is the catalog path plus this suffix.
SIGNATURE_URL_PATH = "/" + SIGNATURE
CATALOG_URL_PATH = "/" + CATALOG


@dataclass(frozen=True, slots=True)
class Key:
    private: Path
    public: Path

    @property
    def pem(self) -> str:
        return self.public.read_text()

    @property
    def entry(self) -> dict[str, str]:
        return {"kind": "key", "key_pem": self.pem}


def make_key(directory: Path) -> Key:
    """A fresh cosign key pair under `directory`; the toolchain's cosign generates it."""
    from tests.fixtures import cosign

    directory.mkdir(parents=True, exist_ok=True)
    try:
        result = cosign.run(
            directory, "generate-key-pair", env={"COSIGN_PASSWORD": KEY_PASSWORD}
        )
    except cosign.CosignUnavailable as unavailable:
        pytest.skip(f"the locked toolchain has no cosign for this host: {unavailable}")
    assert result.returncode == 0, f"cosign generate-key-pair failed: {result.stderr}"
    return Key(directory / "cosign.key", directory / "cosign.pub")


def authority(stack: SigstoreStack) -> str:
    """The `[sigstore."<authority>"]` key of the local stack: the host[:port] of its Fulcio URL."""
    return urlparse(stack.fulcio_url).netloc


def private_authority() -> str:
    """Same as `authority` for callers that hold no stack fixture (the port is env-overridable)."""
    return f"localhost:{os.environ.get('OCX_TEST_FULCIO_PORT', '5555')}"


def stack_table(
    stack: SigstoreStack | None = None, *, trusted_root: str | None = None
) -> str:
    """The `[sigstore."<authority>"]` table that makes the local stack a known instance.

    `trusted_root` overrides the spelling of the root path (e.g. a `file://` URL).
    """
    name = authority(stack) if stack else private_authority()
    root = trusted_root or str((SIGSTORE_DIR / "trusted_root.json").resolve())
    lines = [f'[sigstore."{name}"]', f"trusted_root = {json.dumps(root)}"]
    if stack:
        lines += [
            f'fulcio_url = "{stack.fulcio_url}"',
            f'rekor_url = "{stack.rekor_url}"',
        ]
    return "\n".join(lines) + "\n"


def keyless_entry(
    stack: SigstoreStack, identity: str | None = None, *, instance: bool = True
) -> dict[str, str]:
    """A keyless signer entry for the local stack, naming its instance unless `instance` is off."""
    entry = {
        "kind": "keyless",
        "identity": identity or stack.identity,
        "oidc_issuer": stack.issuer,
    }
    if instance:
        entry["sigstore"] = authority(stack)
    return entry


def toml_inline(value: object) -> str:
    """A TOML value for the shapes these configs use: strings, bools, lists and flat tables."""
    if isinstance(value, dict):
        return (
            "{ "
            + ", ".join(f"{key} = {toml_inline(item)}" for key, item in value.items())
            + " }"
        )
    if isinstance(value, list):
        return "[" + ", ".join(toml_inline(item) for item in value) + "]"
    if isinstance(value, bool):
        return "true" if value else "false"
    return json.dumps(value)  # a JSON string is a valid TOML basic string


def sign_index(
    ocx: OcxRunner,
    directory: Path,
    *,
    registry: str = "ocx.sh",
    key: Key | None = None,
    stack: SigstoreStack | None = None,
    token: Path | None = None,
    expires_in_days: int | None = None,
    extra: tuple[str, ...] = (),
    check: bool = True,
    env_overlay: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx index sign <directory> --registry <registry>` with a key or the local stack.

    Keyless signing needs the stack's table in the home `config.toml` (the self-check after
    signing reads that instance's trust root); callers write it with `configure_index`'s
    `extra_toml` or `stack_table`.
    """
    args = ["index", "sign", str(directory), "--registry", registry]
    overlay = dict(env_overlay or {})
    if key is not None:
        args += ["--key", str(key.private)]
        overlay["OCX_KEY_PASSWORD"] = KEY_PASSWORD
    else:
        assert stack is not None and token is not None, (
            "keyless signing needs the stack and a token file"
        )
        args += [
            "--fulcio-url",
            stack.fulcio_url,
            "--rekor-url",
            stack.rekor_url,
            "--identity-token-file",
            str(token),
            "--no-tty",
        ]
    if expires_in_days is not None:
        args += ["--expires-in-days", str(expires_in_days)]
    args += list(extra)
    result = ocx.run(*args, check=False, env_overlay=overlay)
    if check:
        assert result.returncode == 0, (
            f"index sign failed ({result.returncode})\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )
    return result


def sign_catalog_beyond_the_cli_cap(
    ocx: OcxRunner, directory: Path, key: Key, *, expires_in_days: int
) -> None:
    """Write a catalog whose signed `expires` lies `expires_in_days` ahead, past what `ocx index sign` allows.

    The CLI caps the expiry at 366 days, so a longer one is minted here: `ocx index sign` writes a
    genuine pair for `key`, then the catalog gets the later `expires`, the statement of that bundle
    gets the new digest, and cosign signs the DSSE pre-authentication encoding of the result. The
    bundle keeps the CLI's own key hint, so it is a signature `key` really made.
    """
    from tests.fixtures import cosign

    sign_index(ocx, directory, key=key)
    catalog_path = directory / CATALOG
    signature_path = directory / SIGNATURE
    document = json.loads(catalog_path.read_text())
    expires = datetime.now(UTC) + timedelta(days=expires_in_days)
    document["expires"] = expires.strftime("%Y-%m-%dT%H:%M:%SZ")
    catalog = json.dumps(document).encode()
    catalog_path.write_bytes(catalog)

    bundle = json.loads(signature_path.read_text())
    envelope = bundle["dsseEnvelope"]
    statement = json.loads(base64.b64decode(envelope["payload"]))
    statement["subject"][0]["digest"] = {"sha256": hashlib.sha256(catalog).hexdigest()}
    payload = json.dumps(statement).encode()
    payload_type = envelope["payloadType"].encode()
    pae = b"DSSEv1 %d %b %d %b" % (len(payload_type), payload_type, len(payload), payload)
    (directory / "pae.bin").write_bytes(pae)
    signed = cosign.run(
        directory,
        "sign-blob",
        "--key",
        str(key.private),
        "--use-signing-config=false",
        "--tlog-upload=false",
        "--bundle",
        "pae.sigstore.json",
        "--yes",
        "pae.bin",
        env={"COSIGN_PASSWORD": KEY_PASSWORD},
    )
    assert signed.returncode == 0, f"cosign sign-blob failed: {signed.stderr}"
    raw = json.loads((directory / "pae.sigstore.json").read_text())
    envelope["payload"] = base64.b64encode(payload).decode()
    envelope["signatures"] = [{"sig": raw["messageSignature"]["signature"]}]
    signature_path.write_text(json.dumps(bundle))
    (directory / "pae.bin").unlink()
    (directory / "pae.sigstore.json").unlink()


def configure_index(
    ocx: OcxRunner,
    server: static_index.StaticIndexServer,
    *,
    registry: str = "ocx.sh",
    index_signature: str | None = None,
    index_publishers: list[dict[str, str]] | None = None,
    index_expiry: str | None = None,
    extra_toml: str = "",
) -> Path:
    """Write `config.toml`: `registry` served by `server`, with the index-signature keys given.

    Returns the config path. `extra_toml` is appended verbatim (Sigstore tables, policies).
    """
    host = ocx.registry.split(":", 1)[0]
    lines = [
        f'[registries."{registry}"]',
        f'index = "{server.base_url}"',
        f'trusted_hosts = ["{host}"]',
    ]
    if index_signature is not None:
        lines.append(f'index_signature = "{index_signature}"')
    if index_publishers is not None:
        lines.append(f"index_publishers = {toml_inline(index_publishers)}")
    if index_expiry is not None:
        lines.append(f'index_expiry = "{index_expiry}"')
    path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    path.write_text("\n".join(lines) + "\n" + extra_toml)
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{server.host}"
    return path


def write_index(
    root: Path,
    repositories: list[str],
    *,
    registry: str = "ocx.sh",
    tag: str = "1.0.0",
) -> dict[str, static_index.PackageEntry]:
    """A served index under `root` listing `repositories` (fake physical manifests) in its catalog.

    Enough for `ocx index update`, which reads roots and dispatch objects only. Returns the
    entries by repository.
    """
    static_index.write_config(root)
    entries: dict[str, static_index.PackageEntry] = {}
    for position, repository in enumerate(repositories):
        entries[repository] = static_index.write_package(
            root,
            repository=repository,
            tag=tag,
            physical_repository=f"oci://localhost:1/{repository}",
            platform_digest="sha256:" + f"{position + 1:x}" * 64,
        )
        entries[repository].logical_id = f"{registry}/{repository}:{tag}"
    static_index.write_catalog(
        root, {name: entry.root_digest for name, entry in entries.items()}
    )
    return entries


def error_detail(result: subprocess.CompletedProcess[str]) -> str:
    return json.loads(result.stdout)["error"]["detail"]


def assert_refused(
    result: subprocess.CompletedProcess[str], code: int, slug: str
) -> None:
    assert result.returncode == code, (
        f"expected exit {code} `{slug}`, got {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert error_detail(result) == slug, (
        f"expected detail `{slug}`, got {result.stdout}"
    )


def index_warnings(result: subprocess.CompletedProcess[str]) -> list[str]:
    """The stderr lines that are the index-signature WARN: they name the `index_signature` setting."""
    return [line for line in result.stderr.splitlines() if "index_signature" in line]


def assert_retired_table_refused(ocx: OcxRunner) -> None:
    """The old `[trust.sigstore]` spelling in the home config now fails every command with 78."""
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        '[trust.sigstore]\nrekor_url = "https://rekor.example"\n'
    )
    result = ocx.run("--offline", "index", "list", "ocx.sh/anything", check=False)
    assert_refused(result, 78, "trust_sigstore_retired")
