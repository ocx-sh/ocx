"""The signed-catalog expectation matrix against the public e2e index variants.

`ocx-sh/e2e-index` serves six copies of its index, signed or not by `scripts/sign-catalog.sh`:
`keyless/` (Pages workflow identity), `key/` (key pair, public half in `keys/e2e-index-key.pub`),
`expired/` (`--expires-in-days 0`), `tampered/` (one `packages` value swapped before signing),
`tampered-catalog/` (catalog bytes changed after signing, so the signature no longer matches) and `unsigned/`.
Each case runs `ocx index update` against a variant and asserts the exit code and `error.detail` slug.
Beyond `OCX_E2E_BINARY` the dispatcher needs `OCX_E2E_INDEX_KEY_PUB` (the public key path) and
`OCX_E2E_CHECK_EXPIRY` (the `indexbot check-expiry` command line); CI fails without them, local skips.
"""

from __future__ import annotations

import json
import os
import shlex
import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

from src import previous_release

pytestmark = pytest.mark.e2e_public

REGISTRY = "e2e.ocx.sh"
INDEX_URL = os.environ.get("OCX_E2E_INDEX_URL", "https://ocx-sh.github.io/e2e-index")

#: GitHub repository id of ocx-sh/e2e-index, the claim the keyless signer must carry.
E2E_REPOSITORY_ID = "1455949056"
KEYLESS_IDENTITY = (
    "https://github.com/ocx-sh/e2e-index/.github/workflows/pages.yml@refs/heads/main"
)
ISSUER = "https://token.actions.githubusercontent.com"
#: Roots every variant carries; `tampered/` swaps one catalog value, so the whole set is read.
ROOTS = ["claims", "oid", "none", "tagged", "future"]

Run = Callable[..., subprocess.CompletedProcess[str]]


def required_path(name: str, why: str) -> str:
    value = os.environ.get(name)
    if value:
        return value
    if os.environ.get("CI") or os.environ.get("GITHUB_ACTIONS"):
        pytest.fail(f"{name} unset on CI: {why}")
    pytest.skip(f"{name} unset: {why}")


def keyless_signer(identity: str = KEYLESS_IDENTITY) -> dict[str, object]:
    return {
        "kind": "keyless",
        "identity": identity,
        "oidc_issuer": ISSUER,
        "claims": {"source_repository_identifier": E2E_REPOSITORY_ID},
    }


def key_signer() -> dict[str, str]:
    pem = Path(
        required_path(
            "OCX_E2E_INDEX_KEY_PUB", "the committed public key of the e2e index"
        )
    ).read_text(encoding="utf-8")
    return {"kind": "key", "key_pem": pem}


def toml_value(value: object) -> str:
    if isinstance(value, dict):
        return (
            "{ "
            + ", ".join(f"{key} = {toml_value(item)}" for key, item in value.items())
            + " }"
        )
    if isinstance(value, list):
        return "[" + ", ".join(toml_value(item) for item in value) + "]"
    return json.dumps(value)


@pytest.fixture()
def index_update(e2e_binary: Path, new_client: None, tmp_path: Path) -> Run:
    """`ocx --format json index update` for every root, against one variant, with the given signer list and mode."""

    def run(
        variant: str, mode: str, signers: list[object]
    ) -> subprocess.CompletedProcess[str]:
        home = tmp_path / "ocx-home"
        home.mkdir(exist_ok=True)
        config = tmp_path / f"config-{variant}-{mode}.toml"
        config.write_text(
            "\n".join(
                [
                    f'[registries."{REGISTRY}"]',
                    f'index = "{INDEX_URL}/{variant}"',
                    f'index_signature = "{mode}"',
                    f"index_publishers = {toml_value(signers)}",
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        env = {
            "OCX_HOME": str(home),
            "OCX_CONFIG": str(config),
            "XDG_CONFIG_HOME": str(home / "xdg"),
            "PATH": os.environ.get("PATH", ""),
            "HOME": os.environ.get("HOME", str(Path.home())),
        }
        identifiers = [f"{REGISTRY}/e2e/{root}" for root in ROOTS]
        return subprocess.run(
            [str(e2e_binary), "--format", "json", "index", "update", *identifiers],
            capture_output=True,
            encoding="utf-8",
            env=env,
            stdin=subprocess.DEVNULL,
            check=False,
        )

    return run


def slug(result: subprocess.CompletedProcess[str]) -> str:
    return json.loads(result.stdout)["error"]["detail"]


def warnings(result: subprocess.CompletedProcess[str]) -> list[str]:
    return [line for line in result.stderr.splitlines() if "index_signature" in line]


def signers_for(variant: str) -> list[object]:
    return [key_signer()] if variant == "key" else [keyless_signer()]


@pytest.mark.parametrize("variant", ["keyless", "key"])
@pytest.mark.parametrize("mode", ["warn", "enforce"])
def test_a_variant_signed_by_a_listed_signer_updates_silently(
    index_update: Run, variant: str, mode: str
) -> None:
    result = index_update(variant, mode, signers_for(variant))

    assert result.returncode == 0, (
        f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"
    )
    assert warnings(result) == []


@pytest.mark.parametrize(
    ("variant", "code", "detail"),
    [
        ("expired", 81, "index_catalog_expired"),
        ("tampered", 65, "index_root_digest_mismatch"),
        ("tampered-catalog", 65, "index_signature_invalid"),
        ("unsigned", 79, "index_signature_not_found"),
    ],
)
def test_enforce_refuses_each_broken_variant_with_its_slug(
    index_update: Run, variant: str, code: int, detail: str
) -> None:
    result = index_update(variant, "enforce", [keyless_signer()])

    assert result.returncode == code, (
        f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"
    )
    assert slug(result) == detail


@pytest.mark.parametrize(
    "variant", ["expired", "tampered", "tampered-catalog", "unsigned"]
)
def test_warn_lets_each_broken_variant_through_with_one_warning(
    index_update: Run, variant: str
) -> None:
    result = index_update(variant, "warn", [keyless_signer()])

    assert result.returncode == 0, (
        f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"
    )
    assert len(warnings(result)) == 1, result.stderr


def test_the_keyless_variant_pinned_to_another_identity_is_refused(
    index_update: Run,
) -> None:
    result = index_update(
        "keyless",
        "enforce",
        [
            keyless_signer(
                "https://github.com/someone/else/.github/workflows/x.yml@refs/heads/main"
            )
        ],
    )

    assert result.returncode == 77, (
        f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"
    )
    assert slug(result) == "index_publisher_mismatch"


def test_the_previous_release_installs_from_the_keyless_variant(tmp_path: Path) -> None:
    old = previous_release.resolve()
    home = tmp_path / "old-home"
    home.mkdir()
    (home / "config.toml").write_text(
        f'[registries."{REGISTRY}"]\nindex = "{INDEX_URL}/keyless"\n', encoding="utf-8"
    )
    env = {
        "OCX_HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        "HOME": os.environ.get("HOME", str(Path.home())),
    }

    result = subprocess.run(
        [str(old), "package", "install", f"{REGISTRY}/e2e/none:unsigned"],
        capture_output=True,
        encoding="utf-8",
        env=env,
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=300,
    )

    assert result.returncode == 0, (
        f"previous release refused the signed index: {result.stderr}"
    )


@pytest.mark.parametrize(("variant", "expected"), [("keyless", 0), ("expired", 1)])
def test_the_expiry_alert_script_is_green_on_keyless_and_red_on_expired(
    variant: str, expected: int
) -> None:
    command = shlex.split(
        required_path(
            "OCX_E2E_CHECK_EXPIRY", "the `indexbot check-expiry` command line"
        )
    )

    result = subprocess.run(
        [
            *command,
            "--catalog",
            f"{INDEX_URL}/{variant}/c/index.json",
            "--min-days",
            "21",
        ],
        capture_output=True,
        encoding="utf-8",
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=120,
    )

    assert result.returncode == expected, (
        f"rc={result.returncode}\n{result.stdout}\n{result.stderr}"
    )
