# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``OCX_EXTRA_CA_CERTS`` / ``extra_ca_certs`` /
``extra_ca_certs_pem`` (ocx#448).

Covers the ocx#448 scenarios:

  - **S-002** (every later command trusts the CA): ``ocx index update``
    against an HTTPS index signed by a minted corp CA succeeds with
    ``OCX_EXTRA_CA_CERTS`` set, and fails with a TLS ``UnknownIssuer`` error
    (exit 69) without it. Unit-tier handshake coverage (Windows/macOS
    parity) lives on the Rust side; this module is the CLI-level proof.
  - **S-005** (precedence): ``OCX_EXTRA_CA_CERTS`` wins over every file tier
    a user can edit; an ``OCX_CONFIG`` key wins over ``$OCX_HOME``; a managed
    key wins over the home tier; ``OCX_EXTRA_CA_CERTS=""`` behaves as unset;
    ``ocx.toml`` carrying either key is refused (exit 78, DX-7 — the
    existing ``deny_unknown_fields`` posture). The one exception (ocx#469): a
    pair set in ``/etc/ocx/config.toml`` is system-locked and beats every
    lower tier AND the environment variable, with a warning naming what was
    ignored.
  - **S-007** (misconfiguration is loud and fail-closed):
    ``extra_ca_certs = "<absent>"`` exits 74 with the path in the message, in
    any tier and under ``--offline``; ``OCX_EXTRA_CA_CERTS=/dev/zero`` exits
    74 promptly (D-10 bounded read), never a hang; a ``PRIVATE KEY`` block
    is refused — 78 inline, 65 from a file — and never echoed (D-11).
  - **ocx#467** (the corporate proxy shape): a TLS-terminating ``CONNECT``
    proxy (``src.connect_proxy``) presents the corp CA's leaf for every
    tunnelled origin — the plain index behind it and the real registry
    dialed over https — and the same root flips red to green on both legs;
    a proxy terminating with an unrelated CA stays red with the right root
    configured, and an ``https://`` proxy URL is dialed with the same roots.

Fixture: ``src.tls_index.TlsStaticIndexServer`` wraps
``src.static_index.StaticIndexServer`` in TLS, presenting a leaf certificate
``src.tls_index.mint_ca_and_leaf`` mints and signs with a fresh CA root per
test — the CA PEM is what a test hands to ``OCX_EXTRA_CA_CERTS``. Every
precedence test is a red/green pair on the SAME fixture: the "wrong" CA is a
second, equally valid minted root, so a refusal is never the parser's — it is
the handshake's, and only the tier under test can flip it.
"""

from __future__ import annotations

import contextlib
import json
import os
import socket
import subprocess
import time
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import connect_proxy as connect_proxy_mod
from src import static_index
from src.helpers import PackageInfo, make_package, push_managed_config
from src.registry import fetch_platform_manifest_digest, push_raw_config_package
from src.runner import OcxRunner
from src.tls_index import MintedPki, TlsStaticIndexServer, mint_ca_and_leaf, running_tls

# The logical namespace the HTTPS index serves. Distinct from `ocx.sh` so the
# compiled-in `index.ocx.sh` entry never enters the picture.
_NAMESPACE = "corp.example"

_EXIT_DATA_ERROR = 65
_EXIT_UNAVAILABLE = 69
_EXIT_IO_ERROR = 74
_EXIT_CONFIG_ERROR = 78


@pytest.fixture
def minted_pki() -> MintedPki:
    """A fresh CA root + leaf pair, one per test — never shared, so a test
    asserting on rejection can never accidentally trust a sibling's root.
    """
    return mint_ca_and_leaf()


@pytest.fixture
def wrong_pki() -> MintedPki:
    """A second, unrelated — but equally valid — CA. Configured where a tier
    must LOSE: it parses, it seeds, and it does not sign the fixture's leaf.
    """
    return mint_ca_and_leaf()


@pytest.fixture
def tls_index(tmp_path: Path, minted_pki: MintedPki) -> Iterator[TlsStaticIndexServer]:
    """The HTTPS `index.ocx.sh`-shaped fixture presenting `minted_pki`'s leaf."""
    root = tmp_path / "static_index_root"
    root.mkdir()
    with running_tls(root, minted_pki) as server:
        yield server


@pytest.fixture
def terminator_pki() -> MintedPki:
    """The corp CA a TLS-terminating proxy (ocx#467) presents for every
    tunnelled origin: one leaf for both authorities the tests CONNECT to —
    the index by IP literal and the registry by the name `localhost`."""
    return mint_ca_and_leaf(san_dns=["localhost"])


@pytest.fixture
def plain_index(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    """A plain-HTTP `index.ocx.sh`-shaped origin: behind the terminating
    proxy the origin never speaks TLS itself — the proxy does."""
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


@pytest.fixture
def index_authority() -> str:
    """The `host:port` ocx is told the HTTPS index lives at — a port nothing
    listens on, so only a dial that went through the proxy can ever succeed."""
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return f"127.0.0.1:{probe.getsockname()[1]}"


def _connect_proxy(
    ocx: OcxRunner,
    plain_index: static_index.StaticIndexServer,
    index_authority: str,
    terminator: MintedPki,
    front_tls: MintedPki | None = None,
) -> contextlib.AbstractContextManager[connect_proxy_mod.ConnectProxy]:
    """A terminating proxy routing the phantom index authority onto
    `plain_index` and the registry's own authority onto the registry."""
    index_host, index_port = plain_index.server_address[:2]
    registry_host, registry_port = ocx.registry.split(":", 1)
    return connect_proxy_mod.running(
        {
            index_authority: (index_host, index_port),
            ocx.registry: (registry_host, int(registry_port)),
        },
        terminator,
        front_tls,
    )


@pytest.fixture
def connect_proxy(
    ocx: OcxRunner,
    plain_index: static_index.StaticIndexServer,
    index_authority: str,
    terminator_pki: MintedPki,
) -> Iterator[connect_proxy_mod.ConnectProxy]:
    with _connect_proxy(ocx, plain_index, index_authority, terminator_pki) as proxy:
        yield proxy


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _home_config(ocx: OcxRunner) -> Path:
    return Path(ocx.env["OCX_HOME"]) / "config.toml"


def _index_source_toml(ocx: OcxRunner, server: TlsStaticIndexServer) -> str:
    """The `[registries."corp.example"]` entry pointing at the HTTPS fixture.

    `trusted_hosts` names the loopback test registry's host, the SSRF guard's
    escape hatch every physical deref through the fixture needs (mirrors
    `test_index_ocx_sh.py::configure_index_source`). The fixture's own host is
    deliberately NOT added to `OCX_INSECURE_REGISTRIES`: the index base is
    `https://`, so plain-HTTP allowance has nothing to say about it.
    """
    registry_host = ocx.registry.split(":", 1)[0]
    return f'[registries."{_NAMESPACE}"]\nindex = "{server.base_url}"\ntrusted_hosts = ["{registry_host}"]\n'


def _proxied_index_source_toml(ocx: OcxRunner, index_authority: str) -> str:
    """`_index_source_toml` for the ocx#467 rows: the index base names the
    phantom authority the proxy aliases, never a port anything listens on.
    The index base itself is operator config, not index data, so the SSRF
    floor never judges it; `trusted_hosts` mirrors `_index_source_toml` for
    the physical registry the root document names — `ocx index update` does
    not dial it, but an install through this index would, and on the proxied
    route a loopback NAME is refused (78) without that entry."""
    registry_host = ocx.registry.split(":", 1)[0]
    return f'[registries."{_NAMESPACE}"]\nindex = "https://{index_authority}"\ntrusted_hosts = ["{registry_host}"]\n'


def _pem_toml(pem: bytes) -> str:
    """A root-level `extra_ca_certs_pem` key holding `pem` as a TOML literal
    multi-line string (no escapes to get wrong)."""
    return f"extra_ca_certs_pem = '''\n{pem.decode()}'''\n"


def _publish_through_index(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    server: static_index.StaticIndexServer,
) -> str:
    """Publishes a real package to the loopback registry and a root document
    for it on the HTTPS fixture; returns the logical id `ocx index update`
    resolves through the fixture.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")
    static_index.write_config(server.root)
    repository = f"{unique_repo}/pkg"
    static_index.write_package(
        server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    # `static_index` hardcodes `ocx.sh/...` in its own `logical_id`; the
    # namespace here is the corp one.
    return f"{_NAMESPACE}/{repository}:1.0.0"


def _index_update(
    ocx: OcxRunner, logical_id: str, env_overrides: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return ocx.plain(
        "index", "update", logical_id, check=False, env_overrides=env_overrides
    )


def _assert_unknown_issuer(result: subprocess.CompletedProcess[str], why: str) -> None:
    """The red half: exit 69 and the rustls verdict — not a 404, not a parse
    error, not a refusal of the CA material itself."""
    assert result.returncode == _EXIT_UNAVAILABLE, (
        f"{why}: expected Unavailable(69), got rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "UnknownIssuer" in result.stderr, (
        f"{why}: the failure must be the TLS UnknownIssuer verdict; stderr:\n{result.stderr}"
    )
    assert "extra_ca_certs" in result.stderr and "OCX_EXTRA_CA_CERTS" in result.stderr, (
        f"{why}: the refusal must name the extra-CA remedy; stderr:\n{result.stderr}"
    )


def _assert_resolved(result: subprocess.CompletedProcess[str], why: str) -> None:
    assert result.returncode == 0, (
        f"{why}: rc={result.returncode}\nstderr:\n{result.stderr}"
    )


def _red_then_green(
    ocx: OcxRunner,
    logical_id: str,
    server: static_index.StaticIndexServer,
    green_env: dict[str, str],
) -> None:
    """One fixture, two dials: refused before `green_env`, resolved with it —
    what makes the CA the only variable."""
    red = _index_update(ocx, logical_id)
    _assert_unknown_issuer(red, "without the corp CA")
    assert not server.requests, (
        "a refused handshake must never reach the request handler"
    )

    green = _index_update(ocx, logical_id, env_overrides=green_env)
    _assert_resolved(green, f"with {sorted(green_env)}")
    requested = [record.path for record in server.requests]
    assert any(path.endswith("/config.json") for path in requested), (
        f"the green dial must land the root fetch on the fixture; saw {requested}"
    )


# ---------------------------------------------------------------------------
# S-002: every later command trusts the CA
# ---------------------------------------------------------------------------


def test_index_update_red_without_ca_green_with_ca_path(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    minted_pki: MintedPki,
    tls_index: TlsStaticIndexServer,
) -> None:
    """S-002: `ocx index update` against the HTTPS index fails `UnknownIssuer`
    (exit 69) without `OCX_EXTRA_CA_CERTS`, and succeeds once the variable
    names the corp CA's PEM file (D-4 path form)."""
    _home_config(ocx).write_text(_index_source_toml(ocx, tls_index))
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, tls_index)
    ca_path = tmp_path / "corp-ca.pem"
    ca_path.write_bytes(minted_pki.ca_cert_pem)

    _red_then_green(ocx, logical_id, tls_index, {"OCX_EXTRA_CA_CERTS": str(ca_path)})


def test_index_update_green_with_inline_pem_in_env(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    minted_pki: MintedPki,
    tls_index: TlsStaticIndexServer,
) -> None:
    """S-002 / D-4: the same variable carrying the PEM TEXT (sniffed on
    `-----BEGIN`) is the inline form — same red, same green."""
    _home_config(ocx).write_text(_index_source_toml(ocx, tls_index))
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, tls_index)

    _red_then_green(
        ocx,
        logical_id,
        tls_index,
        {"OCX_EXTRA_CA_CERTS": minted_pki.ca_cert_pem.decode()},
    )


def test_index_update_green_with_config_pem_key(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    minted_pki: MintedPki,
    tls_index: TlsStaticIndexServer,
) -> None:
    """S-002: "with that config" — the persisted `extra_ca_certs_pem` form in
    `$OCX_HOME/config.toml` (what `ocx self setup` writes, S-001) is trusted
    by a later command with NO environment variable at all."""
    config = _home_config(ocx)
    config.write_text(_index_source_toml(ocx, tls_index))
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, tls_index)

    _assert_unknown_issuer(_index_update(ocx, logical_id), "before the key is written")

    config.write_text(
        _pem_toml(minted_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )
    _assert_resolved(
        _index_update(ocx, logical_id),
        "with extra_ca_certs_pem in $OCX_HOME/config.toml",
    )


# ---------------------------------------------------------------------------
# S-005: precedence
# ---------------------------------------------------------------------------


def test_env_wins_over_every_config_file_tier(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    minted_pki: MintedPki,
    wrong_pki: MintedPki,
    tls_index: TlsStaticIndexServer,
) -> None:
    """S-005: `OCX_EXTRA_CA_CERTS` outranks a conflicting key in BOTH file
    tiers a user can edit at once — `$OCX_HOME` and an `OCX_CONFIG` file
    each carry a valid-but-wrong CA. Red without the variable proves the
    wrong CA is really wrong; green with it proves the env value is honoured.
    The reverse direction — env wrong, both file tiers right — must then be
    refused: that is what tells "replaced" apart from "joined", since a union
    would resolve either way. The system tier is the one file the variable
    does NOT outrank (ocx#469, `test_system_tier_lock_beats_every_lower_tier_and_env`)."""
    explicit = tmp_path / "explicit-config.toml"
    explicit.write_text(_pem_toml(wrong_pki.ca_cert_pem))
    _home_config(ocx).write_text(
        _pem_toml(wrong_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )
    ocx.env["OCX_CONFIG"] = str(explicit)
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, tls_index)
    ca_path = tmp_path / "corp-ca.pem"
    ca_path.write_bytes(minted_pki.ca_cert_pem)

    _red_then_green(ocx, logical_id, tls_index, {"OCX_EXTRA_CA_CERTS": str(ca_path)})

    # Reverse direction: the file tiers now hold the right CA and the
    # variable the wrong one — only a replacing env value can lose here.
    wrong_path = tmp_path / "wrong-ca.pem"
    wrong_path.write_bytes(wrong_pki.ca_cert_pem)
    explicit.write_text(_pem_toml(minted_pki.ca_cert_pem))
    _home_config(ocx).write_text(
        _pem_toml(minted_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )
    _assert_unknown_issuer(
        _index_update(
            ocx, logical_id, env_overrides={"OCX_EXTRA_CA_CERTS": str(wrong_path)}
        ),
        "env (wrong) over both file tiers (right)",
    )


def test_explicit_config_key_wins_over_home_config(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    minted_pki: MintedPki,
    wrong_pki: MintedPki,
    tls_index: TlsStaticIndexServer,
) -> None:
    """S-005: an `OCX_CONFIG` file's key wins over the same concern in
    `$OCX_HOME/config.toml` — in both directions. The home tier holds the
    inline form and the explicit file the path form, so the win also
    exercises the XOR merge (a higher tier's path form clears a lower tier's
    `_pem`, never joins it): were both honoured, the "wrong" direction would
    still resolve."""
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, tls_index)
    right_path = tmp_path / "corp-ca.pem"
    right_path.write_bytes(minted_pki.ca_cert_pem)
    wrong_path = tmp_path / "wrong-ca.pem"
    wrong_path.write_bytes(wrong_pki.ca_cert_pem)
    explicit = tmp_path / "explicit-config.toml"
    ocx.env["OCX_CONFIG"] = str(explicit)

    # Direction 1: home wrong (inline), explicit right (path) → resolves.
    _home_config(ocx).write_text(
        _pem_toml(wrong_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )
    explicit.write_text(f'extra_ca_certs = "{right_path}"\n')
    _assert_resolved(
        _index_update(ocx, logical_id), "explicit (right) over home (wrong)"
    )

    # Direction 2: home right (inline), explicit wrong (path) → refused. The
    # home tier's correct CA is present and parses; only precedence can make
    # it lose.
    _home_config(ocx).write_text(
        _pem_toml(minted_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )
    explicit.write_text(f'extra_ca_certs = "{wrong_path}"\n')
    _assert_unknown_issuer(
        _index_update(ocx, logical_id), "explicit (wrong) over home (right)"
    )


def test_managed_key_wins_over_the_home_file_tier(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    minted_pki: MintedPki,
    wrong_pki: MintedPki,
    tls_index: TlsStaticIndexServer,
) -> None:
    """S-005: the `[managed]` tier's `extra_ca_certs_pem` outranks the
    `$OCX_HOME` `config.toml` tier — the same precedence order every other
    `[managed]`-vs-home-tier key follows. (It does not outrank a
    system-locked pair, ocx#469 — the loader unit test owns that direction.)

    The managed payload is published with `src.helpers.push_managed_config`
    (the product path, `ocx config push`) and adopted through the
    invocation-only `OCX_MANAGED_CONFIG` override, so the file tier is
    never rewritten by the adoption. Red without the override (home tier
    wrong, no managed tier) — green with it (managed right). The reverse
    direction — home tier right, a SECOND managed payload wrong — must then
    be refused: only a REPLACING (not unioned) managed tier can lose there,
    since a union would still resolve through the right home tier.
    """
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, tls_index)
    _home_config(ocx).write_text(
        _pem_toml(wrong_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )

    managed_repo = f"{unique_repo}_managed"
    push_managed_config(
        ocx, managed_repo, "corp", _pem_toml(minted_pki.ca_cert_pem), tmp_path
    )
    ref = f"{ocx.registry}/{managed_repo}:corp"
    adopt = ocx.run(
        "config", "update", check=False, env_overrides={"OCX_MANAGED_CONFIG": ref}
    )
    assert adopt.returncode == 0, (
        f"adopting the managed payload must succeed: {adopt.stderr}"
    )

    _red_then_green(ocx, logical_id, tls_index, {"OCX_MANAGED_CONFIG": ref})

    # Reverse direction: the home tier now holds the RIGHT CA and a SECOND
    # managed payload the WRONG one — a union of managed-and-file would still
    # resolve here, so only replacement can produce the refusal.
    _home_config(ocx).write_text(
        _pem_toml(minted_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )
    wrong_managed_repo = f"{unique_repo}_managed_wrong"
    push_managed_config(
        ocx, wrong_managed_repo, "corp", _pem_toml(wrong_pki.ca_cert_pem), tmp_path
    )
    ref_wrong = f"{ocx.registry}/{wrong_managed_repo}:corp"
    adopt_wrong = ocx.run(
        "config",
        "update",
        check=False,
        env_overrides={"OCX_MANAGED_CONFIG": ref_wrong},
    )
    assert adopt_wrong.returncode == 0, (
        f"adopting the (wrong) managed payload must succeed: {adopt_wrong.stderr}"
    )
    _assert_unknown_issuer(
        _index_update(ocx, logical_id, {"OCX_MANAGED_CONFIG": ref_wrong}),
        "managed (wrong) over home (right)",
    )


def test_system_tier_lock_beats_every_lower_tier_and_env(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    minted_pki: MintedPki,
    wrong_pki: MintedPki,
    tls_index: TlsStaticIndexServer,
) -> None:
    """ocx#469: a pair set in `/etc/ocx/config.toml` (reached through the
    `__OCX_TESTING_SYSTEM_CONFIG` seam) is system-locked — it beats a
    `$OCX_HOME` pair AND `OCX_EXTRA_CA_CERTS`, both pointing at a
    valid-but-wrong root, and says so on stderr without echoing either value
    (D-11). The inverse first: without the system pair the same home pair
    and env value are honoured and refuse, so the green below is the lock's
    and not a fixture that never disagreed."""
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, tls_index)
    wrong_path = tmp_path / "wrong-ca.pem"
    wrong_path.write_bytes(wrong_pki.ca_cert_pem)
    system = tmp_path / "system-config.toml"
    ocx.env["__OCX_TESTING_SYSTEM_CONFIG"] = str(system)
    _home_config(ocx).write_text(
        _pem_toml(wrong_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )
    wrong_env = {"OCX_EXTRA_CA_CERTS": str(wrong_path)}

    _assert_unknown_issuer(
        _index_update(ocx, logical_id, env_overrides=wrong_env),
        "without the system pair: env (wrong) over home (wrong)",
    )

    system.write_text(_pem_toml(minted_pki.ca_cert_pem))
    green = _index_update(ocx, logical_id, env_overrides=wrong_env)
    _assert_resolved(green, "system (right, locked) over home (wrong) and env (wrong)")
    remedy = f"locked by {system}; edit the system tier or ask its owner"
    home_ignored = f"ignoring extra_ca_certs / extra_ca_certs_pem from $OCX_HOME/config.toml: the pair is {remedy}"
    env_ignored = (
        f"ignoring OCX_EXTRA_CA_CERTS: extra_ca_certs / extra_ca_certs_pem are {remedy}"
    )
    for ignored in (home_ignored, env_ignored):
        assert ignored in green.stderr, (
            f"the lock must say what it ignored: {ignored!r}\nstderr:\n{green.stderr}"
        )
    assert str(wrong_path) not in green.stderr, (
        f"the ignored env value must never be echoed (D-11): {green.stderr}"
    )


def test_empty_env_value_behaves_as_unset(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    minted_pki: MintedPki,
    tls_index: TlsStaticIndexServer,
) -> None:
    """S-005: `OCX_EXTRA_CA_CERTS=""` behaves as unset — the configured
    file-tier value still applies (and the empty string is neither read as a
    path — 74 — nor parsed as inline text — 78)."""
    _home_config(ocx).write_text(
        _pem_toml(minted_pki.ca_cert_pem) + _index_source_toml(ocx, tls_index)
    )
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, tls_index)

    result = _index_update(ocx, logical_id, env_overrides={"OCX_EXTRA_CA_CERTS": ""})
    _assert_resolved(
        result, 'OCX_EXTRA_CA_CERTS="" with the CA in $OCX_HOME/config.toml'
    )


@pytest.mark.parametrize(
    "key_line",
    [
        pytest.param('extra_ca_certs = "corp-ca.pem"\n', id="path-key"),
        pytest.param(
            "extra_ca_certs_pem = '''\n-----BEGIN CERTIFICATE-----\n'''\n",
            id="inline-key",
        ),
    ],
)
def test_ocx_toml_carrying_either_key_is_refused(
    ocx: OcxRunner, tmp_path: Path, key_line: str
) -> None:
    """S-005 / DX-7: `ocx.toml` carrying `extra_ca_certs` or
    `extra_ca_certs_pem` is refused as an unknown key (exit 78) — the project
    file is not a trust-material tier. `lock` is the command that parses the
    project file (`test_project_config.py`'s 78 precedent)."""
    project = tmp_path / "ocx.toml"
    project.write_text(key_line)

    result = ocx.plain("--offline", "--project", str(project), "lock", check=False)
    assert result.returncode == _EXIT_CONFIG_ERROR, (
        f"ocx.toml with an extra-CA key must exit ConfigError(78); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "extra_ca_certs" in result.stderr, (
        f"the refusal must name the offending key: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# S-007: misconfiguration is loud and fail-closed
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("tier", ["home", "system"])
def test_nonexistent_path_exits_74_with_path_in_message(
    ocx: OcxRunner, tmp_path: Path, tier: str
) -> None:
    """S-007: `extra_ca_certs = "<absent>"` in any tier exits 74 with the
    KEY and the TIER named in the message, not merely the bare path — for a
    command that never dials anything."""
    absent = tmp_path / "absent-corp-ca.pem"
    assert not absent.exists()
    line = f'extra_ca_certs = "{absent}"\n'
    if tier == "home":
        _home_config(ocx).write_text(line)
    else:
        system = tmp_path / "system-config.toml"
        system.write_text(line)
        ocx.env["__OCX_TESTING_SYSTEM_CONFIG"] = str(system)

    result = ocx.plain("index", "catalog", check=False)
    assert result.returncode == _EXIT_IO_ERROR, (
        f"an absent extra_ca_certs path must exit IoError(74); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert f"extra_ca_certs={absent}" in result.stderr, (
        f"the refusal must name the key, not only the path: {result.stderr}"
    )
    tier_label = "$OCX_HOME/config.toml" if tier == "home" else "system config.toml"
    assert tier_label in result.stderr, (
        f"the refusal must name the {tier} tier: {result.stderr}"
    )


def test_nonexistent_path_exits_74_under_offline(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """S-007 / D-9: the same refusal fires under `--offline` — validation
    needs no network, and an offline run never builds a client with fewer
    roots than configured. The message names the key, not merely the bare
    path."""
    absent = tmp_path / "absent-corp-ca.pem"
    _home_config(ocx).write_text(f'extra_ca_certs = "{absent}"\n')

    result = ocx.plain("--offline", "index", "catalog", check=False)
    assert result.returncode == _EXIT_IO_ERROR, (
        f"--offline must still exit IoError(74) on an absent path; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert f"extra_ca_certs={absent}" in result.stderr, (
        f"the refusal must name the key, not only the path: {result.stderr}"
    )


def test_dev_zero_env_path_exits_74_promptly(ocx: OcxRunner) -> None:
    """S-007 / D-10: `OCX_EXTRA_CA_CERTS=/dev/zero` exits 74 promptly — the
    bounded read (`read_bounded`) refuses a non-regular file rather than
    streaming an endless device into memory."""
    started = time.monotonic()
    result = ocx.plain(
        "--offline",
        "index",
        "catalog",
        check=False,
        env_overrides={"OCX_EXTRA_CA_CERTS": "/dev/zero"},
    )
    elapsed = time.monotonic() - started

    assert result.returncode == _EXIT_IO_ERROR, (
        f"/dev/zero must exit IoError(74); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert elapsed < 5, (
        f"the refusal must be prompt (bounded read), took {elapsed:.1f}s"
    )
    assert "/dev/zero" in result.stderr, (
        f"the refusal must name the path: {result.stderr}"
    )


@pytest.mark.skipif(not hasattr(os, "mkfifo"), reason="FIFOs are POSIX")
def test_fifo_env_path_exits_74_promptly(ocx: OcxRunner, tmp_path: Path) -> None:
    """S-007 / D-10: `OCX_EXTRA_CA_CERTS` naming a FIFO exits 74 promptly —
    the bounded reader (`read_bounded`) refuses a non-regular file before
    ever opening it, rather than blocking forever on `open(2)` with no
    writer on the other end."""
    fifo = tmp_path / "corp-ca.fifo"
    os.mkfifo(fifo)

    try:
        result = subprocess.run(
            [str(ocx.binary), "--format", "json", "--offline", "index", "catalog"],
            env={**ocx.env, "OCX_EXTRA_CA_CERTS": str(fifo)},
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=5,
            check=False,
        )
    except subprocess.TimeoutExpired:
        pytest.fail(
            "ocx hung on a FIFO named by OCX_EXTRA_CA_CERTS instead of refusing it"
        )

    assert result.returncode == _EXIT_IO_ERROR, (
        f"a FIFO path must exit IoError(74); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "corp-ca.fifo" in result.stderr, (
        f"the refusal must name the path: {result.stderr}"
    )
    assert "not a regular file" in result.stderr, (
        f"the refusal must state why the FIFO is refused: {result.stderr}"
    )


def test_env_private_key_block_exits_78_and_never_echoes_it(
    ocx: OcxRunner, minted_pki: MintedPki
) -> None:
    """S-007 / D-10 / D-11: an inline `PRIVATE KEY` block in the variable is
    refused (78 — inline material is a configuration fault), never silently
    skipped, and the key's body never reaches stderr."""
    key_pem = minted_pki.leaf_key_pem.decode()
    result = ocx.plain(
        "--offline",
        "index",
        "catalog",
        check=False,
        env_overrides={"OCX_EXTRA_CA_CERTS": key_pem},
    )

    assert result.returncode == _EXIT_CONFIG_ERROR, (
        f"an inline PRIVATE KEY must exit ConfigError(78); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "OCX_EXTRA_CA_CERTS" in result.stderr, (
        f"the refusal must name the variable: {result.stderr}"
    )
    body = key_pem.splitlines()[1]
    assert body not in result.stderr, (
        "a pasted secret must never be echoed into the diagnostic (D-11)"
    )


def test_config_path_to_private_key_file_exits_65(
    ocx: OcxRunner, tmp_path: Path, minted_pki: MintedPki
) -> None:
    """S-007 / C-010: the same refusal from a FILE the config names is the
    file's data being wrong — 65 — and the message names the key and the
    path, never the contents."""
    key_path = tmp_path / "not-a-ca.pem"
    key_path.write_bytes(minted_pki.leaf_key_pem)
    _home_config(ocx).write_text(f'extra_ca_certs = "{key_path}"\n')

    result = ocx.plain("--offline", "index", "catalog", check=False)
    assert result.returncode == _EXIT_DATA_ERROR, (
        f"a PRIVATE KEY file must exit DataError(65); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert f"extra_ca_certs={key_path}" in result.stderr, (
        f"the refusal must name the key, not only the path: {result.stderr}"
    )
    body = minted_pki.leaf_key_pem.decode().splitlines()[1]
    assert body not in result.stderr, (
        "file contents must never be echoed into the diagnostic (D-11)"
    )


def test_system_tier_inline_private_key_exits_78_naming_the_system_tier(
    ocx: OcxRunner, tmp_path: Path, minted_pki: MintedPki
) -> None:
    """S-007 / DX-16: an inline `PRIVATE KEY` block in the SYSTEM tier's
    `extra_ca_certs_pem` is refused (78 — inline material is a
    configuration fault) and the message names both the key and the system
    tier — never the key's body (D-11)."""
    system = tmp_path / "system-config.toml"
    system.write_text(_pem_toml(minted_pki.leaf_key_pem))
    ocx.env["__OCX_TESTING_SYSTEM_CONFIG"] = str(system)

    result = ocx.plain("--offline", "index", "catalog", check=False)

    assert result.returncode == _EXIT_CONFIG_ERROR, (
        f"an inline PRIVATE KEY in the system tier must exit ConfigError(78); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "extra_ca_certs_pem (system config.toml)" in result.stderr, (
        f"the refusal must name the key and the system tier: {result.stderr}"
    )
    body = minted_pki.leaf_key_pem.decode().splitlines()[1]
    assert body not in result.stderr, (
        "a pasted secret must never be echoed into the diagnostic (D-11)"
    )


def test_managed_payload_path_form_is_dropped_with_a_warning(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """S-004 / C-002: a managed payload naming `extra_ca_certs` in the PATH
    form is dropped by the loader — a path on the publisher's disk names
    nothing on a fleet machine — with a logged warning, and the command
    that reads the managed tier still succeeds (never 74 for a path only
    the publisher's disk has)."""
    managed_repo = f"{unique_repo}_managed_path_form"
    push_raw_config_package(
        ocx.registry,
        managed_repo,
        "corp",
        b'extra_ca_certs = "/nonexistent/corp-ca.pem"\n',
    )
    ref = f"{ocx.registry}/{managed_repo}:corp"

    adopt = ocx.run(
        "config", "update", check=False, env_overrides={"OCX_MANAGED_CONFIG": ref}
    )
    assert adopt.returncode == 0, (
        f"adopting the managed payload must succeed despite the dropped key: {adopt.stderr}"
    )

    result = ocx.plain(
        "--offline",
        "index",
        "catalog",
        check=False,
        env_overrides={"OCX_MANAGED_CONFIG": ref},
    )
    assert result.returncode == 0, (
        f"a path only the publisher's disk has must never fail the consumer: "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "set extra_ca_certs to a local path; ignored" in result.stderr, (
        f"the drop must be logged as a warning: {result.stderr}"
    )


def test_managed_payload_pem_this_host_cannot_load_is_refused_at_adoption(
    ocx: OcxRunner, unique_repo: str, minted_pki: MintedPki
) -> None:
    """H1 (review r2, DX-20): `ocx config push` proves a bundle on the
    publisher's platform only, so a consumer re-runs the check with its own
    verifier when it would adopt the snapshot. A payload whose
    `extra_ca_certs_pem` this host cannot load is refused (78, naming the
    managed tier and the block, never the bytes), the previous snapshot stays
    in force, and every command keeps running — `config update` included,
    so a corrected payload is one more push away, never a hand-deleted
    snapshot.

    Pushed raw (never through `ocx config push`, which would refuse it) and
    with a bundle no platform's verifier loads, so the property holds on
    every leg; the platform-divergent root the finding names (v2, webpki
    only) is the unit tier's `extra_ca_parse_pem_refuses_a_pre_v3_root_…`.
    """
    managed_repo = f"{unique_repo}_managed_pem_refused"
    ref = f"{ocx.registry}/{managed_repo}:corp"
    env = {"OCX_MANAGED_CONFIG": ref}
    snapshot_path = (
        Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json"
    )

    push_raw_config_package(
        ocx.registry, managed_repo, "corp", _pem_toml(minted_pki.ca_cert_pem).encode()
    )
    adopt = ocx.run("config", "update", check=False, env_overrides=env)
    assert adopt.returncode == 0, (
        f"positive control: a loadable bundle adopts: {adopt.stderr}"
    )
    previous = snapshot_path.read_text()

    unusable = "-----BEGIN CERTIFICATE-----\nbm90IGEgY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n"
    push_raw_config_package(
        ocx.registry, managed_repo, "corp", _pem_toml(unusable.encode()).encode()
    )
    refused = ocx.run("config", "update", check=False, env_overrides=env)
    assert refused.returncode == _EXIT_CONFIG_ERROR, (
        f"a bundle this host cannot load must be refused at adoption (78): "
        f"rc={refused.returncode}\nstderr:\n{refused.stderr}"
    )
    assert "previous snapshot is kept" in refused.stderr, refused.stderr
    assert "extra_ca_certs_pem (managed config)" in refused.stderr, refused.stderr
    assert "block 1 does not parse" in refused.stderr, refused.stderr
    assert "bm90IGEgY2VydGlmaWNhdGU" not in refused.stderr, "never the bytes (D-11)"
    assert snapshot_path.read_text() == previous, "the previous snapshot stays in force"

    result = ocx.plain("--offline", "index", "catalog", check=False, env_overrides=env)
    assert result.returncode == 0, (
        f"the host keeps running on the previous snapshot: rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# D-8: the variable is not forwarded
# ---------------------------------------------------------------------------


def test_env_var_is_not_forwarded_across_exec_clean(
    ocx: OcxRunner, tmp_path: Path, minted_pki: MintedPki
) -> None:
    """D-8: `OCX_EXTRA_CA_CERTS` is deliberately absent from the forwarded
    `OCX_*` set (`apply_ocx_config`), so a `--clean` child does not see it —
    a CA is public material a child re-reads from disk for itself, and an
    inline PEM would exceed Windows' per-variable limit. The inherited
    (non-`--clean`) form is the positive control: the same variable, the
    same child, present only because the ambient environment carried it."""
    (tmp_path / "ocx.toml").write_text("[tools]\n")
    project = ("--offline", "--project", str(tmp_path))
    ocx.plain(*project, "lock")
    env = {"OCX_EXTRA_CA_CERTS": minted_pki.ca_cert_pem.decode()}
    # An absolute interpreter: `--clean` drops the inherited PATH.
    probe = ("/bin/sh", "-c", 'printf "%s" "${OCX_EXTRA_CA_CERTS:-unset}"')

    clean = ocx.plain(*project, "exec", "--clean", "--", *probe, env_overrides=env)
    assert clean.stdout == "unset", (
        f"a --clean child must not receive OCX_EXTRA_CA_CERTS (D-8); got {clean.stdout!r}"
    )

    inherited = ocx.plain(*project, "exec", "--", *probe, env_overrides=env)
    assert inherited.stdout == minted_pki.ca_cert_pem.decode(), (
        "positive control: without --clean the ambient variable reaches the child"
    )


# ---------------------------------------------------------------------------
# ocx#467: a TLS-terminating CONNECT proxy
# ---------------------------------------------------------------------------


def test_index_update_through_a_terminating_connect_proxy(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    terminator_pki: MintedPki,
    plain_index: static_index.StaticIndexServer,
    index_authority: str,
    connect_proxy: connect_proxy_mod.ConnectProxy,
) -> None:
    """ocx#467, index leg: with `HTTPS_PROXY` naming a proxy that terminates
    TLS with the corp CA, `ocx index update` is refused `UnknownIssuer` (69)
    until `OCX_EXTRA_CA_CERTS` names that CA — and the fetch then lands on
    the plain origin behind the proxy, through a tunnel to an authority
    nothing listens on directly."""
    _home_config(ocx).write_text(_proxied_index_source_toml(ocx, index_authority))
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, plain_index)
    ocx.env["HTTPS_PROXY"] = connect_proxy.url
    ca_path = tmp_path / "corp-ca.pem"
    ca_path.write_bytes(terminator_pki.ca_cert_pem)

    _red_then_green(ocx, logical_id, plain_index, {"OCX_EXTRA_CA_CERTS": str(ca_path)})

    # Exactly one refused tunnel: the index client's retry ladder excludes
    # a verifier's verdict — it re-dialed this three times before it did.
    refused = connect_proxy.refused_handshakes
    assert refused == [index_authority], (
        f"the red dial must have reached the proxy once and refused its leaf; {refused=}"
    )
    assert set(connect_proxy.tunnels) == {index_authority}, (
        f"every tunnel must be the index authority; saw {connect_proxy.tunnels}"
    )
    assert len(connect_proxy.tunnels) > len(refused), (
        "the green dial must have tunnelled too"
    )


def test_package_pull_through_a_terminating_connect_proxy(
    ocx: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
    terminator_pki: MintedPki,
    connect_proxy: connect_proxy_mod.ConnectProxy,
) -> None:
    """ocx#467, registry leg: a real `ocx package pull` dialed over https —
    `OCX_INSECURE_REGISTRIES` deliberately absent from the environment —
    tunnels `CONNECT localhost:<registry port>` through the proxy, whose
    leaf names `localhost`; red 69 `UnknownIssuer` without the corp root,
    green with it, and the object store then holds the package."""
    pkg = published_package
    env = {
        key: value for key, value in ocx.env.items() if key != "OCX_INSECURE_REGISTRIES"
    }
    env["HTTPS_PROXY"] = connect_proxy.url
    ca_path = tmp_path / "corp-ca.pem"
    ca_path.write_bytes(terminator_pki.ca_cert_pem)

    def pull(extra: dict[str, str]) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(ocx.binary), "--format", "json", "package", "pull", pkg.fq],
            env={**env, **extra},
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=120,
            check=False,
        )

    red = pull({})
    _assert_unknown_issuer(red, "https pull through the proxy, no corp CA")
    # Exactly one tunnel: the pull path never re-dials a refused handshake,
    # and 69 keeps it that way — a retry keyed on the transient bucket
    # (`push_blob`'s, a CI wrapper's) has nothing to key on.
    refused = connect_proxy.refused_handshakes
    assert refused == [ocx.registry], (
        f"the red pull must have tunnelled to the registry once and refused the leaf; {refused=}"
    )

    green = pull({"OCX_EXTRA_CA_CERTS": str(ca_path)})
    _assert_resolved(green, "https pull through the proxy with the corp CA")
    assert set(connect_proxy.tunnels) == {ocx.registry}, (
        f"every tunnel must be the registry authority; saw {connect_proxy.tunnels}"
    )
    root = Path(json.loads(green.stdout)[pkg.fq])
    assert (root / "metadata.json").is_file(), (
        f"pull must materialise the package at {root}"
    )


def test_a_proxy_terminating_with_an_unrelated_ca_stays_refused(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    terminator_pki: MintedPki,
    wrong_pki: MintedPki,
    plain_index: static_index.StaticIndexServer,
    index_authority: str,
) -> None:
    """ocx#467, negative: the RIGHT root is configured but the proxy
    terminates with a leaf an unrelated CA signed — still 69 `UnknownIssuer`.
    Trust follows the presented chain; an extra root does not turn every
    intercepting proxy into a trusted one."""
    _home_config(ocx).write_text(_proxied_index_source_toml(ocx, index_authority))
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, plain_index)
    ca_path = tmp_path / "corp-ca.pem"
    ca_path.write_bytes(terminator_pki.ca_cert_pem)

    with _connect_proxy(ocx, plain_index, index_authority, wrong_pki) as proxy:
        ocx.env["HTTPS_PROXY"] = proxy.url
        _assert_unknown_issuer(
            _index_update(ocx, logical_id, {"OCX_EXTRA_CA_CERTS": str(ca_path)}),
            "right root configured, proxy terminating with an unrelated CA",
        )
        assert proxy.refused_handshakes == [index_authority], proxy.refused_handshakes
    assert not plain_index.requests, "a refused handshake must never reach the origin"


def test_an_https_proxy_url_is_dialed_with_the_same_roots(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    terminator_pki: MintedPki,
    plain_index: static_index.StaticIndexServer,
    index_authority: str,
) -> None:
    """ocx#467: `HTTPS_PROXY=https://localhost:<port>` — the proxy itself is
    dialed over TLS, presenting the same corp leaf, and the tunnel's TLS
    runs inside that session. Red: the PROXY handshake is refused, so no
    CONNECT is ever sent; green: both layers verify against the one root."""
    _home_config(ocx).write_text(_proxied_index_source_toml(ocx, index_authority))
    logical_id = _publish_through_index(ocx, unique_repo, tmp_path, plain_index)
    ca_path = tmp_path / "corp-ca.pem"
    ca_path.write_bytes(terminator_pki.ca_cert_pem)

    with _connect_proxy(
        ocx, plain_index, index_authority, terminator_pki, front_tls=terminator_pki
    ) as proxy:
        assert proxy.url.startswith("https://localhost:"), proxy.url
        ocx.env["HTTPS_PROXY"] = proxy.url

        _assert_unknown_issuer(
            _index_update(ocx, logical_id), "https proxy, no corp CA"
        )
        assert not proxy.tunnels, (
            f"a refused proxy handshake must precede any CONNECT; saw {proxy.tunnels}"
        )

        _assert_resolved(
            _index_update(ocx, logical_id, {"OCX_EXTRA_CA_CERTS": str(ca_path)}),
            "https proxy with the corp CA",
        )
        assert set(proxy.tunnels) == {index_authority}, proxy.tunnels
        assert not proxy.refused_handshakes, proxy.refused_handshakes
    requested = [record.path for record in plain_index.requests]
    assert any(path.endswith("/config.json") for path in requested), requested
