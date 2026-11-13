# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for publisher trust: an index root's `publishers` list.

A package is *declared* when its committed index root carries a non-empty
`publishers` array. Install, pull and exec of a declared package verify against
that list with no local configuration; an operator `[[trust.policy]]` is a second,
independent round and both must pass.

Cases run against the local Sigstore stack (keyless) or a per-test cosign key
(never committed), behind a static `index.ocx.sh`-shaped server over zot.
"""
from __future__ import annotations

import json
import subprocess
import time
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

import pytest

from src import static_index
from src.helpers import make_package, rglob_no_links
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import cosign
from tests.fixtures.sigstore_stack import SigstoreStack
from tests.test_attest import cyclonedx_predicate

KEY_PASSWORD = "ocxtest-publisher"
SKIPPED_WARNING = "signature verification skipped for policy-covered package(s) via --no-verify / OCX_NO_VERIFY"
BAD_IDENTITY = "not-a-listed-publisher@example.com"


# ──────────────────────────────────────────────────────────────────────────────
# Fixtures and helpers
# ──────────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class Key:
    private: Path
    public: Path

    @property
    def pem(self) -> str:
        return self.public.read_text()

    @property
    def entry(self) -> dict:
        return {"kind": "key", "key_pem": self.pem}


def _make_key(directory: Path) -> Key:
    directory.mkdir(parents=True, exist_ok=True)
    result = cosign.run(directory, "generate-key-pair", env={"COSIGN_PASSWORD": KEY_PASSWORD})
    assert result.returncode == 0, f"cosign generate-key-pair failed: {result.stderr}"
    return Key(directory / "cosign.key", directory / "cosign.pub")


@pytest.fixture()
def key_a(tmp_path: Path) -> Key:
    return _make_key(tmp_path / "key_a")


@pytest.fixture()
def key_b(tmp_path: Path) -> Key:
    return _make_key(tmp_path / "key_b")


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    static_index.write_config(root)
    with static_index.running(root) as server:
        yield server


@dataclass(slots=True)
class Declared:
    """A package published to the zot registry and indexed by the static server."""

    pkg: PackageInfo
    entry: static_index.PackageEntry
    repository: str
    leaf_digest: str

    @property
    def logical(self) -> str:
        return self.entry.logical_id


def keyless(stack: SigstoreStack, identity: str | None = None) -> dict:
    return {"kind": "keyless", "identity": identity or stack.identity, "oidc_issuer": stack.issuer}


def configure(
    ocx: OcxRunner,
    server: static_index.StaticIndexServer,
    *,
    signatures: str | None = None,
    policy: str = "",
    home: Path | None = None,
) -> None:
    """Write `config.toml`: the `ocx.sh` index source, an optional `signatures`, an operator policy."""
    host = ocx.registry.split(":", 1)[0]
    text = f'[registries."ocx.sh"]\nindex = "{server.base_url}"\ntrusted_hosts = ["{host}"]\n'
    if signatures is not None:
        text += f'signatures = "{signatures}"\n'
    text += policy
    ((home or Path(ocx.env["OCX_HOME"])) / "config.toml").write_text(text)
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{server.host}"


def publish(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    server: static_index.StaticIndexServer,
    publishers: object = None,
    *,
    present: bool = False,
    signatures: str | None = None,
    policy: str = "",
) -> Declared:
    """Publish a package, index it under `ocx.sh/<repo>/pkg` with `publishers`, point ocx at it."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    configure(ocx, server, signatures=signatures, policy=policy)
    repository = f"{unique_repo}/pkg"
    entry = write_root(server, repository, pkg, ocx, leaf_digest, publishers, present=present)
    return Declared(pkg, entry, repository, leaf_digest)


def write_root(
    server: static_index.StaticIndexServer,
    repository: str,
    pkg: PackageInfo,
    ocx: OcxRunner,
    leaf_digest: str,
    publishers: object,
    *,
    present: bool = False,
) -> static_index.PackageEntry:
    os_name, arch_name = pkg.platform.split("/")
    return static_index.write_package(
        server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
        publishers=publishers,
        publishers_present=present,
    )


def trust_env(stack: SigstoreStack) -> dict[str, str]:
    """Pin the local stack's Rekor key and CA; no Sigstore-services request is possible."""
    return {"OCX_SIGSTORE_TRUSTED_ROOT": str(stack.trusted_root_json)}


#: Everything that is not the loopback registry or index is unreachable, so a
#: key-only install that needs Sigstore services fails instead of passing quietly.
SIGSTORE_UNREACHABLE = {
    "HTTPS_PROXY": "http://127.0.0.1:9",
    "https_proxy": "http://127.0.0.1:9",
    "NO_PROXY": "127.0.0.1,localhost",
    "no_proxy": "127.0.0.1,localhost",
}


def sign_keyless(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo) -> None:
    result = ocx.run("package", "sign", *stack.sign_args(token), pkg.short, check=False)
    assert result.returncode == 0, f"keyless sign setup failed: {result.stderr}"


def sign_key(ocx: OcxRunner, key: Key, pkg: PackageInfo) -> None:
    result = ocx.run(
        "package", "sign", "--platform", current_platform(), "--key", str(key.private), pkg.short,
        check=False, env_overlay={"OCX_KEY_PASSWORD": KEY_PASSWORD},
    )
    assert result.returncode == 0, f"key sign setup failed: {result.stderr}"


def attest_keyless(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo, predicate: Path) -> None:
    result = ocx.run(
        "package", "attest", *stack.sign_args(token), "--predicate", str(predicate), "--type", "cyclonedx",
        pkg.short, check=False,
    )
    assert result.returncode == 0, f"keyless attest setup failed: {result.stderr}"


def attest_key(ocx: OcxRunner, key: Key, pkg: PackageInfo, predicate: Path) -> None:
    result = ocx.run(
        "package", "attest", "--platform", current_platform(), "--key", str(key.private),
        "--predicate", str(predicate), "--type", "cyclonedx", pkg.short,
        check=False, env_overlay={"OCX_KEY_PASSWORD": KEY_PASSWORD},
    )
    assert result.returncode == 0, f"key attest setup failed: {result.stderr}"


def install(
    ocx: OcxRunner, ref: str, *, stack: SigstoreStack | None = None, flags: tuple[str, ...] = (),
    env: dict[str, str] | None = None, log_level: str | None = None, verb: str = "install",
) -> subprocess.CompletedProcess[str]:
    overlay = {**(trust_env(stack) if stack else {}), **(env or {})}
    return ocx.run(
        "package", verb, "-p", current_platform(), *flags, ref,
        check=False, env_overlay=overlay, log_level=log_level,
    )


def exec_hello(
    ocx: OcxRunner, ref: str, *, stack: SigstoreStack | None = None, flags: tuple[str, ...] = (),
    env: dict[str, str] | None = None, fmt: str | None = None,
) -> subprocess.CompletedProcess[str]:
    overlay = {**(trust_env(stack) if stack else {}), **(env or {})}
    return ocx.run(
        "package", "exec", "-p", current_platform(), *flags, ref, "--", "hello",
        format=fmt, check=False, env_overlay=overlay,
    )


def detail(result: subprocess.CompletedProcess[str]) -> str:
    return json.loads(result.stdout)["error"]["detail"]


def assert_refused(result: subprocess.CompletedProcess[str], code: int, slug: str) -> None:
    assert result.returncode == code, (
        f"expected exit {code} `{slug}`, got {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert detail(result) == slug, f"expected detail `{slug}`, got {result.stdout}"


def assert_nothing_installed(ocx: OcxRunner) -> None:
    packages = ocx.ocx_home / "packages"
    assert not packages.exists() or not list(rglob_no_links(packages, "metadata.json")), (
        "a refused install must leave no assembled package"
    )


def assert_installed(ocx: OcxRunner) -> None:
    packages = ocx.ocx_home / "packages"
    assert packages.exists() and list(rglob_no_links(packages, "metadata.json")), "package was not installed"


# ──────────────────────────────────────────────────────────────────────────────
# The declared release is verified against the root's list
# ──────────────────────────────────────────────────────────────────────────────


def test_s001_release_signed_by_a_listed_publisher_installs_without_warning(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    """Discriminated on the same fixture: the listed identity is the only difference."""
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert result.returncode == 0, result.stderr
    assert_installed(ocx)
    assert "warn" not in result.stderr.lower(), f"a verified install is silent:\n{result.stderr}"


def test_s002_release_signed_by_another_signer_is_refused_with_publisher_mismatch(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack, BAD_IDENTITY)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert_refused(result, 77, "publisher_mismatch")
    assert_nothing_installed(ocx)


def test_s002_after_rotation_a_committed_root_message_names_index_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack, BAD_IDENTITY)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)
    ocx.plain("index", "update", f"ocx.sh/{d.repository}")

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert_refused(result, 77, "publisher_mismatch")
    assert f"ocx index update {d.repository}" in result.stdout + result.stderr


def test_s003_unsigned_release_of_a_declared_package_is_refused_with_no_signatures_found(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert_refused(result, 79, "no_signatures_found")
    assert_nothing_installed(ocx)


# ──────────────────────────────────────────────────────────────────────────────
# A root without publishers installs as today
# ──────────────────────────────────────────────────────────────────────────────


def test_s004_undeclared_package_installs_with_the_info_line(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server)  # no `publishers` key

    result = install(ocx, d.logical, log_level="info")
    assert result.returncode == 0, result.stderr
    assert "without signature verification" in result.stderr
    assert f"no trust policy covers 'ocx.sh/{d.repository}'" in result.stderr


def test_s004_required_refuses_an_undeclared_package_with_publishers_required(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, signatures="required")

    result = install(ocx, d.logical)
    assert_refused(result, 81, "publishers_required")
    assert_nothing_installed(ocx)


# ──────────────────────────────────────────────────────────────────────────────
# Entries this ocx cannot read, and malformed lists
# ──────────────────────────────────────────────────────────────────────────────


def test_s005_a_list_of_only_unrecognised_entries_is_refused_with_publishers_unrecognized(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [{"kind": "future"}])

    result = install(ocx, d.logical)
    assert_refused(result, 82, "publishers_unrecognized")
    assert_nothing_installed(ocx)


def test_s005_a_usable_entry_beside_an_unrecognised_one_installs_when_it_matches(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [{"kind": "future"}, keyless(sigstore_stack)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert result.returncode == 0, result.stderr
    assert_installed(ocx)


@pytest.mark.parametrize(
    "publishers",
    [None, {"kind": "keyless"}, "keyless", [], [{"kind": "keyless", "oidc_issuer": "x"}]],
    ids=["null", "object", "string", "empty-array", "keyless-without-matcher"],
)
def test_malformed_publishers_are_refused_with_publishers_invalid(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, publishers: object,
) -> None:
    """`"publishers": null` and every other malformed shape refuse 65, fail-closed."""
    d = publish(ocx, unique_repo, tmp_path, index_server, publishers, present=True)

    result = install(ocx, d.logical)
    assert_refused(result, 65, "publishers_invalid")
    assert_nothing_installed(ocx)


# ──────────────────────────────────────────────────────────────────────────────
# A key publisher needs no Sigstore network
# ──────────────────────────────────────────────────────────────────────────────


def test_s006_key_publisher_installs_with_sigstore_unreachable_and_no_trust_root(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, key_a: Key,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [key_a.entry])
    sign_key(ocx, key_a, d.pkg)

    result = install(ocx, d.logical, env=SIGSTORE_UNREACHABLE)  # no OCX_SIGSTORE_TRUSTED_ROOT
    assert result.returncode == 0, result.stderr
    assert_installed(ocx)


def test_s006_a_signature_by_a_different_key_is_refused_with_publisher_mismatch(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, key_a: Key, key_b: Key,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [key_a.entry])
    sign_key(ocx, key_b, d.pkg)

    result = install(ocx, d.logical, env=SIGSTORE_UNREACHABLE)
    assert_refused(result, 77, "publisher_mismatch")
    assert_nothing_installed(ocx)


# ──────────────────────────────────────────────────────────────────────────────
# Publishers beside an operator `[[trust.policy]]`
# ──────────────────────────────────────────────────────────────────────────────


def _key_policy(scope: str, key: Key) -> str:
    return f'\n[[trust.policy]]\nscope = "{scope}"\nsigners = [{{ kind = "key", key = "{key.public}" }}]\n'


def test_s007_install_needs_both_the_publisher_and_the_operator_signature(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path, key_a: Key,
) -> None:
    scope = f"ocx.sh/{unique_repo}/pkg"
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)], policy=_key_policy(scope, key_a))
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)
    sign_key(ocx, key_a, d.pkg)

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert result.returncode == 0, result.stderr
    assert_installed(ocx)


def test_s007_missing_operator_signature_is_refused_with_identity_mismatch(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path, key_a: Key,
) -> None:
    scope = f"ocx.sh/{unique_repo}/pkg"
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)], policy=_key_policy(scope, key_a))
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)  # P signed, K did not

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert_refused(result, 77, "identity_mismatch")
    assert_nothing_installed(ocx)


def test_s007_missing_publisher_signature_is_refused_with_publisher_mismatch(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, key_a: Key,
) -> None:
    scope = f"ocx.sh/{unique_repo}/pkg"
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)], policy=_key_policy(scope, key_a))
    sign_key(ocx, key_a, d.pkg)  # K signed, P did not

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert_refused(result, 77, "publisher_mismatch")
    assert_nothing_installed(ocx)


def test_s008_signatures_off_checks_only_the_operator_signature(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, key_a: Key,
) -> None:
    """The root lists a keyless publisher nobody signed as; only K's signature is demanded."""
    scope = f"ocx.sh/{unique_repo}/pkg"
    d = publish(
        ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)],
        signatures="off", policy=_key_policy(scope, key_a),
    )
    sign_key(ocx, key_a, d.pkg)

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert result.returncode == 0, result.stderr
    assert_installed(ocx)


def test_s008_signatures_off_still_enforces_the_operator_policy(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, key_a: Key,
) -> None:
    scope = f"ocx.sh/{unique_repo}/pkg"
    d = publish(
        ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)],
        signatures="off", policy=_key_policy(scope, key_a),
    )  # unsigned

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert_refused(result, 79, "no_signatures_found")


# ──────────────────────────────────────────────────────────────────────────────
# Opt-out
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("via", ["flag", "env"])
def test_s009_no_verify_installs_an_unsigned_declared_package_with_one_warning(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, via: str,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])  # signed by nobody

    flags = ("--no-verify",) if via == "flag" else ()
    env = {"OCX_NO_VERIFY": "1"} if via == "env" else None
    result = install(ocx, d.logical, flags=flags, env=env)
    assert result.returncode == 0, result.stderr
    assert result.stderr.count(SKIPPED_WARNING) == 1, result.stderr
    assert_installed(ocx)


def test_s009_no_verify_installs_past_a_corrupt_committed_root(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    ocx.plain("index", "update", f"ocx.sh/{d.repository}")
    roots = [p for p in (ocx.ocx_home / "index").rglob("pkg.json") if p.parent.name == unique_repo]
    assert len(roots) == 1, f"expected one committed root, got {roots}"
    roots[0].write_text("{ not json")

    pinned = f"{d.logical}@{d.leaf_digest}"
    refused = install(ocx, pinned)
    assert refused.returncode != 0, f"a corrupt committed root must refuse without the opt-out:\n{refused.stderr}"
    assert_nothing_installed(ocx)

    roots[0].write_text("{ not json")  # the refused run may have healed it
    result = install(ocx, pinned, flags=("--no-verify",))
    assert result.returncode == 0, result.stderr
    assert result.stderr.count(SKIPPED_WARNING) == 1, result.stderr
    assert_installed(ocx)


# ──────────────────────────────────────────────────────────────────────────────
# An installed declared package carries its origin marker
# ──────────────────────────────────────────────────────────────────────────────


def test_s010_exec_of_an_installed_declared_package_makes_no_index_request(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)
    first = install(ocx, d.logical, stack=sigstore_stack)
    assert first.returncode == 0, first.stderr
    assert index_server.requests, "positive control: the install must have reached the index"
    index_server.requests.clear()

    online = exec_hello(ocx, d.logical, stack=sigstore_stack)
    assert online.returncode == 0, online.stderr
    assert [(r.method, r.path) for r in index_server.requests] == [], "exec of installed content hit the index"

    frozen = exec_hello(ocx, d.logical, stack=sigstore_stack, env={"OCX_FROZEN": "1"})
    assert frozen.returncode == 0, frozen.stderr
    assert index_server.requests == [], "frozen exec hit the index"

    # No registry request counter exists in the harness: with the network forbidden,
    # a single registry request would fail the exec.
    offline = exec_hello(ocx, d.logical, stack=sigstore_stack, env={"OCX_OFFLINE": "1"})
    assert offline.returncode == 0, offline.stderr
    assert index_server.requests == []


def test_s010_a_lock_pinned_exec_of_an_installed_declared_package_makes_no_index_request(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)
    assert install(ocx, d.logical, stack=sigstore_stack).returncode == 0
    project = _locked_project(ocx, tmp_path, d)
    assert (project / "ocx.lock").is_file(), "positive control: the lock pins the declared package"
    assert index_server.requests, "positive control: setup must have reached the index"
    index_server.requests.clear()

    for env in ({"OCX_FROZEN": "1"}, {"OCX_OFFLINE": "1"}):
        result = subprocess.run(
            [str(ocx.binary), "exec", "--", "hello"], cwd=project, capture_output=True, text=True,
            env={**ocx.env, **trust_env(sigstore_stack), **env}, stdin=subprocess.DEVNULL, check=False,
        )
        assert result.returncode == 0, f"{env}: {result.stderr}"
        assert index_server.requests == [], f"{env}: lock-pinned exec hit the index"


# ──────────────────────────────────────────────────────────────────────────────
# A root that adds a publisher
# ──────────────────────────────────────────────────────────────────────────────


def test_s014_a_fresh_home_accepts_a_release_signed_by_either_listed_publisher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(
        ocx, unique_repo, tmp_path, index_server,
        [keyless(sigstore_stack, BAD_IDENTITY), keyless(sigstore_stack)],
    )
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)

    result = install(ocx, d.logical, stack=sigstore_stack)
    assert result.returncode == 0, result.stderr


def test_s014_a_committed_home_widens_only_after_index_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack, BAD_IDENTITY)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)
    ocx.plain("index", "update", f"ocx.sh/{d.repository}")  # commit the narrow root
    write_root(
        index_server, d.repository, d.pkg, ocx, d.leaf_digest,
        [keyless(sigstore_stack, BAD_IDENTITY), keyless(sigstore_stack)],
    )

    before = install(ocx, d.logical, stack=sigstore_stack)
    assert_refused(before, 77, "publisher_mismatch")  # a fetched root never widens a committed list

    ocx.plain("index", "update", f"ocx.sh/{d.repository}")
    after = install(ocx, d.logical, stack=sigstore_stack)
    assert after.returncode == 0, after.stderr


# ──────────────────────────────────────────────────────────────────────────────
# A future `signatures` value is treated as required
# ──────────────────────────────────────────────────────────────────────────────


def test_s015_unknown_signatures_value_behaves_as_required_with_one_warning(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, signatures="strict")

    result = install(ocx, d.logical)
    assert_refused(result, 81, "publishers_required")
    warnings = [line for line in result.stderr.splitlines() if "strict" in line]
    assert len(warnings) == 1, f"expected one warning naming `strict`, got {warnings}"


# ──────────────────────────────────────────────────────────────────────────────
# A lockfile pin in a fresh home
# ──────────────────────────────────────────────────────────────────────────────


def _locked_project(ocx: OcxRunner, tmp_path: Path, d: Declared) -> Path:
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\nhello = "{d.logical}"\n')
    locked = subprocess.run(
        [str(ocx.binary), "lock", "--no-pull"], cwd=project, capture_output=True, text=True,
        env=ocx.env, stdin=subprocess.DEVNULL, check=False,
    )
    assert locked.returncode == 0, locked.stderr
    return project


def _fresh_home_exec(
    ocx: OcxRunner, project: Path, tmp_path: Path, server, *, signatures: str | None, log_level: str | None = None,
    policy: str = "", fmt: str | None = None,
) -> subprocess.CompletedProcess[str]:
    home = tmp_path / "fresh_home"
    home.mkdir()
    configure(ocx, server, signatures=signatures, policy=policy, home=home)
    argv = [
        str(ocx.binary), *(["--format", fmt] if fmt else []),
        *(["--log-level", log_level] if log_level else []), "exec", "--", "hello",
    ]
    return subprocess.run(
        argv, cwd=project, capture_output=True, text=True, env={**ocx.env, "OCX_HOME": str(home)},
        stdin=subprocess.DEVNULL, check=False,
    )


def test_s016_a_locked_declared_package_in_a_fresh_home_installs_undeclared_with_the_info_line(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])  # signed by nobody
    project = _locked_project(ocx, tmp_path, d)

    result = _fresh_home_exec(ocx, project, tmp_path, index_server, signatures=None, log_level="info")
    assert result.returncode == 0, result.stderr
    assert "without signature verification" in result.stderr


def test_s016_required_in_a_fresh_home_refuses_naming_index_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    project = _locked_project(ocx, tmp_path, d)

    result = _fresh_home_exec(ocx, project, tmp_path, index_server, signatures="required", fmt="json")
    assert_refused(result, 81, "publishers_required")
    assert f"ocx index update {d.repository}" in result.stderr


def test_s016_operator_policy_still_applies_to_a_locked_package_in_a_fresh_home(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, key_a: Key,
) -> None:
    scope = f"ocx.sh/{unique_repo}/pkg"
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])  # unsigned
    project = _locked_project(ocx, tmp_path, d)

    result = _fresh_home_exec(
        ocx, project, tmp_path, index_server, signatures=None, policy=_key_policy(scope, key_a), fmt="json",
    )
    assert_refused(result, 79, "no_signatures_found")


def test_s016_operator_policy_refuses_a_wrong_signer_for_a_locked_package_in_a_fresh_home(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, key_a: Key, key_b: Key,
) -> None:
    scope = f"ocx.sh/{unique_repo}/pkg"
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    sign_key(ocx, key_b, d.pkg)
    project = _locked_project(ocx, tmp_path, d)

    result = _fresh_home_exec(
        ocx, project, tmp_path, index_server, signatures=None, policy=_key_policy(scope, key_a), fmt="json",
    )
    # An operator key policy reads another key's signature as one that does not verify (65), as it does today.
    assert_refused(result, 65, "signature_invalid")


# ──────────────────────────────────────────────────────────────────────────────
# The origin gate: content installed elsewhere is verified under the declared name
# ──────────────────────────────────────────────────────────────────────────────


def test_s017_content_installed_under_another_name_is_verified_before_a_declared_exec(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack, BAD_IDENTITY)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)
    # Same digest, installed through the plain registry name: no declaration applies there.
    plain = install(ocx, d.pkg.short)
    assert plain.returncode == 0, plain.stderr

    result = exec_hello(ocx, d.logical, stack=sigstore_stack, fmt="json")
    assert_refused(result, 77, "publisher_mismatch")


def test_s017_the_same_setup_runs_when_the_listed_publisher_signed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    """Control for the refusal above: only the listed identity differs."""
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    sign_keyless(ocx, sigstore_stack, identity_token, d.pkg)
    assert install(ocx, d.pkg.short).returncode == 0

    result = exec_hello(ocx, d.logical, stack=sigstore_stack)
    assert result.returncode == 0, result.stderr


# ──────────────────────────────────────────────────────────────────────────────
# Attestations are held to both rounds
# ──────────────────────────────────────────────────────────────────────────────


def _sbom(tmp_path: Path, name: str, version: int) -> Path:
    path = tmp_path / name
    path.write_text(json.dumps({"bomFormat": "CycloneDX", "specVersion": "1.6", "version": version, "components": []}))
    return path


def _sbom_cmd(ocx: OcxRunner, d: Declared, stack: SigstoreStack, output: Path) -> subprocess.CompletedProcess[str]:
    return ocx.run(
        "package", "sbom", "-p", current_platform(), "--type", "cyclonedx", "--output", str(output), d.logical,
        check=False, env_overlay=trust_env(stack),
    )


def test_s011_two_sboms_by_a_listed_signer_yield_the_newer(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    attest_keyless(ocx, sigstore_stack, identity_token, d.pkg, _sbom(tmp_path, "old.json", 1))
    time.sleep(2.5)  # the log time has second resolution
    attest_keyless(ocx, sigstore_stack, identity_token, d.pkg, _sbom(tmp_path, "new.json", 2))

    out = tmp_path / "out.json"
    result = _sbom_cmd(ocx, d, sigstore_stack, out)
    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads(out.read_text())["version"] == 2, "the newer SBOM must win"


def test_s011_an_unlogged_key_candidate_among_them_is_refused_as_multiple_attestations(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path, key_a: Key,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack), key_a.entry])
    attest_keyless(ocx, sigstore_stack, identity_token, d.pkg, _sbom(tmp_path, "logged.json", 1))
    attest_key(ocx, key_a, d.pkg, _sbom(tmp_path, "unlogged.json", 2))

    result = _sbom_cmd(ocx, d, sigstore_stack, tmp_path / "out.json")
    assert_refused(result, 65, "multiple_attestations")


def test_s012_attestation_by_another_signer_is_refused_with_publisher_mismatch(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack, BAD_IDENTITY)])
    attest_keyless(ocx, sigstore_stack, identity_token, d.pkg, cyclonedx_predicate(tmp_path))

    result = ocx.run(
        "package", "verify", "--attestation", "-p", current_platform(), d.logical,
        check=False, env_overlay=trust_env(sigstore_stack),
    )
    assert_refused(result, 77, "publisher_mismatch")


def test_s012_an_explicit_identity_flag_replaces_the_publisher_round(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack, BAD_IDENTITY)])
    attest_keyless(ocx, sigstore_stack, identity_token, d.pkg, cyclonedx_predicate(tmp_path))

    result = ocx.run(
        "package", "verify", "--attestation", "-p", current_platform(),
        "--certificate-identity", sigstore_stack.identity, "--certificate-oidc-issuer", sigstore_stack.issuer,
        d.logical, check=False, env_overlay=trust_env(sigstore_stack),
    )
    assert result.returncode == 0, result.stdout + result.stderr


def test_r5_a_publisher_signed_and_an_operator_signed_attestation_are_each_ineligible(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path, key_a: Key,
) -> None:
    """Document A passes only the publisher round, B only the operator round: neither verifies."""
    scope = f"ocx.sh/{unique_repo}/pkg"
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)], policy=_key_policy(scope, key_a))
    attest_keyless(ocx, sigstore_stack, identity_token, d.pkg, _sbom(tmp_path, "a.json", 1))
    attest_key(ocx, key_a, d.pkg, _sbom(tmp_path, "b.json", 2))

    result = ocx.run(
        "package", "verify", "--attestation", "-p", current_platform(), d.logical,
        check=False, env_overlay=trust_env(sigstore_stack),
    )
    assert_refused(result, 79, "no_signatures_found")  # no document verified in every round


def test_r5_control_without_the_operator_policy_the_publisher_signed_document_verifies(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path, key_a: Key,
) -> None:
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    attest_keyless(ocx, sigstore_stack, identity_token, d.pkg, _sbom(tmp_path, "a.json", 1))
    attest_key(ocx, key_a, d.pkg, _sbom(tmp_path, "b.json", 2))

    result = ocx.run(
        "package", "verify", "--attestation", "-p", current_platform(), d.logical,
        check=False, env_overlay=trust_env(sigstore_stack),
    )
    assert result.returncode == 0, result.stdout + result.stderr


def test_a_failed_index_resolve_refuses_sbom_instead_of_reading_without_publishers(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server, sigstore_stack, identity_token: Path,
) -> None:
    """A fresh home holds no root, so a resolve that fails once must not read as "no publishers declared"."""
    d = publish(ocx, unique_repo, tmp_path, index_server, [keyless(sigstore_stack)])
    attest_keyless(ocx, sigstore_stack, identity_token, d.pkg, _sbom(tmp_path, "a.json", 1))
    # 403 is not retried by the index ladder, so the one refusal reaches the command.
    index_server.transient_refusals[f"/p/{d.repository}.json"] = [403]

    refused = _sbom_cmd(ocx, d, sigstore_stack, tmp_path / "refused.json")
    assert refused.returncode != 0, f"a failed resolve ran the command without the publisher round: {refused.stderr}"
    assert not (tmp_path / "refused.json").exists()

    # Control: the same command once the origin answers.
    served = _sbom_cmd(ocx, d, sigstore_stack, tmp_path / "served.json")
    assert served.returncode == 0, served.stdout + served.stderr
