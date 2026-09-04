# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests: the SSRF guard must honour the HTTP proxy route
(ocx-sh/ocx#407, ocx-sh/ocx#323).

Design record: `.claude/state/plans/bugfix_plan_proxy_ssrf_guard.md` rows
A/B/C. These three tests must be RED on `1957a91b` (pre-fix HEAD) and GREEN
once work packages A-D land the route-aware guard.

Why the forward proxy is named by HOSTNAME (`forward_proxy.url`, never
`forward_proxy.address`): `hyper_util`'s proxy matcher resolves an IP-literal
proxy address without ever invoking the custom DNS-resolver hook the SSRF
guard installs, so a proxy dialed by IP literal would prove nothing about the
hook's admission behaviour under a NAME-based proxy (ocx-sh/ocx#323's actual
shape — a corporate proxy configured by hostname).

Why `PHANTOM_REGISTRY` (`no-such-registry.invalid:5000`, from
`src/forward_proxy.py`): RFC 6761 SS2.4 reserves the `.invalid` TLD to never
resolve. That is a guarantee, not an observation, so case A cannot pass by
accident because the destination host happened to become resolvable — only
because the guard actually stopped resolving it and let the proxy dial it
instead.

`OcxRunner` builds each invocation's environment from scratch
(`OcxRunner.__init__` / `run(env_overrides=...)`, never inheriting the
ambient process environment beyond a fixed allow-list) — a developer running
these tests from behind a real corporate proxy cannot leak `HTTP_PROXY` into
cases B and C, which assert the DIRECT-route and forbidden-literal refusals
still fire correctly on the isolated environment these tests construct.
"""

from __future__ import annotations

import http.client
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import forward_proxy as forward_proxy_mod
from src import static_index
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Fixture: a local `index.ocx.sh`-shaped HTTP server, one per test.
#
# Function-scoped and file-local, mirroring `test_index_ocx_sh.py::index_server`
# exactly (that file defines its own copy rather than sharing one via
# `test/conftest.py` — the same convention this file follows).
# ---------------------------------------------------------------------------


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


def _configure_index_source_without_trust(
    ocx: OcxRunner, server: static_index.StaticIndexServer
) -> None:
    """Points `[registries."ocx.sh"] index` at the fixture with **no**
    `trusted_hosts` entry — unlike `test_index_ocx_sh.py::configure_index_source`,
    this omission is load-bearing here (plan row A / SS2.4 of the sibling design
    notes): it forces the guarded per-namespace physical-fetch client to run
    its SSRF pre-flight against the proxied destination instead of skipping it
    via the trust escape hatch, so a fix missing the resolver-hook admission
    half fails with a distinct, diagnosable message rather than passing for
    the wrong reason.
    """
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(f'[registries."ocx.sh"]\nindex = "{server.base_url}"\n')


# ---------------------------------------------------------------------------
# Fixture self-test — drives `ForwardProxy` directly with `http.client`, no
# `ocx` involved. Covers the two behaviours only a throwaway smoke script
# proved during implementation.
# ---------------------------------------------------------------------------


def test_the_fixture_refuses_origin_form_and_relays_an_aliased_get_status(
    forward_proxy: forward_proxy_mod.ForwardProxy,
) -> None:
    """An origin-form request refuses with 400, an unimplemented method (POST)
    answers 501 via the base handler's default, and an aliased absolute-form
    GET reaches the real registry and relays its upstream status.
    """
    host, port = forward_proxy.server_address[:2]

    origin_form = http.client.HTTPConnection(host, port, timeout=5)
    origin_form.request("GET", "/v2/")
    assert origin_form.getresponse().status == 400
    origin_form.close()

    unimplemented_method = http.client.HTTPConnection(host, port, timeout=5)
    unimplemented_method.request(
        "POST", f"http://{forward_proxy_mod.PHANTOM_REGISTRY}/x"
    )
    assert unimplemented_method.getresponse().status == 501
    unimplemented_method.close()

    aliased_get = http.client.HTTPConnection(host, port, timeout=5)
    aliased_get.request("GET", f"http://{forward_proxy_mod.PHANTOM_REGISTRY}/v2/")
    assert aliased_get.getresponse().status == 200
    aliased_get.close()


# ---------------------------------------------------------------------------
# Row A — ocx-sh/ocx#407: a proxied pull must reach a registry host the
# process itself can never resolve.
# ---------------------------------------------------------------------------


def test_a_proxied_pull_reaches_a_registry_the_host_cannot_resolve(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    forward_proxy: forward_proxy_mod.ForwardProxy,
) -> None:
    """Plan row A / ocx-sh/ocx#407: under `HTTP_PROXY`/`HTTPS_PROXY`, the
    physical registry name is text in the proxy's request line, never
    resolved by the ocx process — the SSRF pre-flight must see the route as
    `Proxied` and skip its own `lookup_host`, and `GuardedResolver` must admit
    the proxy's own hostname (`localhost`) so the real TCP connect can
    proceed.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{forward_proxy_mod.PHANTOM_REGISTRY}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )

    _configure_index_source_without_trust(ocx, index_server)
    ocx.env["OCX_INSECURE_REGISTRIES"] = (
        f"{ocx.registry},{forward_proxy_mod.PHANTOM_REGISTRY},{index_server.host}"
    )
    proxied_env = {"HTTP_PROXY": forward_proxy.url, "HTTPS_PROXY": forward_proxy.url}

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain(
        "--index",
        str(index_dir),
        "package",
        "install",
        entry.logical_id,
        env_overrides=proxied_env,
    )
    result = ocx.plain(
        "--offline",
        "--index",
        str(index_dir),
        "package",
        "exec",
        entry.logical_id,
        "--",
        "hello",
        env_overrides=proxied_env,
    )

    assert result.returncode == 0
    assert pkg.marker in result.stdout

    assert forward_proxy.requests, "the proxy must have carried at least one request"
    assert forward_proxy_mod.PHANTOM_REGISTRY in forward_proxy.authorities()


# ---------------------------------------------------------------------------
# Row B — `NO_PROXY` sends the same destination direct again; the refusal
# must classify as exit 69 (Unavailable), not 78.
# ---------------------------------------------------------------------------


def test_no_proxy_sends_the_pull_direct_and_the_refusal_is_unavailable(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    forward_proxy: forward_proxy_mod.ForwardProxy,
) -> None:
    """Plan row B / ocx-sh/ocx#407: naming the destination in `NO_PROXY`
    routes it direct, where the destination is genuinely unresolvable
    (RFC 6761 `.invalid`) — the SSRF pre-flight's `Resolution` failure must
    classify as exit 69 (Unavailable), not the pre-fix 78 (ConfigError) that
    `oci/index/error.rs` gave every `SsrfError` variant alike.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{forward_proxy_mod.PHANTOM_REGISTRY}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )

    _configure_index_source_without_trust(ocx, index_server)
    ocx.env["OCX_INSECURE_REGISTRIES"] = (
        f"{ocx.registry},{forward_proxy_mod.PHANTOM_REGISTRY},{index_server.host}"
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain(
        "--index",
        str(index_dir),
        "package",
        "install",
        entry.logical_id,
        check=False,
        env_overrides={
            "HTTP_PROXY": forward_proxy.url,
            "HTTPS_PROXY": forward_proxy.url,
            "NO_PROXY": "no-such-registry.invalid",
        },
    )

    assert result.returncode == 69, (
        f"expected exit 69 (Unavailable), got rc={result.returncode}\n{result.stderr}"
    )
    assert "failed to resolve host no-such-registry.invalid" in result.stderr

    assert forward_proxy.requests, (
        "the index fetch must still have gone through the proxy"
    )
    assert forward_proxy_mod.PHANTOM_REGISTRY not in forward_proxy.authorities(), (
        "NO_PROXY must have kept the phantom registry off the proxy's authority log"
    )


# ---------------------------------------------------------------------------
# Row C — guard, not regression: a forbidden IP literal stays refused even
# on a proxied route. Green today, green after.
# ---------------------------------------------------------------------------


def test_a_forbidden_ip_literal_is_refused_even_on_a_proxied_route(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    forward_proxy: forward_proxy_mod.ForwardProxy,
) -> None:
    """Plan row C / ocx-sh/ocx#407 — a GUARD, deliberately green both before
    and after the fix. The DNS-lookup skip a proxied route grants a hostname
    destination must never extend to a forbidden IP LITERAL: `guard_destination`
    refuses those textually regardless of route, so this must keep failing
    with exit 78 throughout. `127.0.0.1:<registry port>` is the real,
    already-published registry — if the literal floor were ever dropped for
    a proxied route, the pull would *succeed* and this test would red on the
    exit code, which is exactly the discrimination this case exists for.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")
    registry_port = ocx.registry.split(":", 1)[1]

    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://127.0.0.1:{registry_port}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )

    _configure_index_source_without_trust(ocx, index_server)
    ocx.env["OCX_INSECURE_REGISTRIES"] = (
        f"{ocx.registry},127.0.0.1:{registry_port},{index_server.host}"
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain(
        "--index",
        str(index_dir),
        "package",
        "install",
        entry.logical_id,
        check=False,
        env_overrides={
            "HTTP_PROXY": forward_proxy.url,
            "HTTPS_PROXY": forward_proxy.url,
        },
    )

    assert result.returncode == 78, (
        f"expected exit 78 (ForbiddenTarget), got rc={result.returncode}\n{result.stderr}"
    )
    assert "resolves to a forbidden address 127.0.0.1" in result.stderr
