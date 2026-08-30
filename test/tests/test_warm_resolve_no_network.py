# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the warm-resolve contract: with a hosted index
configured, a command whose every byte is already snapshotted locally contacts
the index source ZERO times.

Regression for the per-invocation index traffic caused by
``ChainedIndex::physical_reference`` walking the sources before reading the
committed local root: every identifier-resolving command (install, exec, which,
add, lock, pull, run, env) fired a ``GET /config.json`` (the jurisdiction
declaration) plus a ``GET /p/<ns>/<pkg>.json`` (the root document) on every warm
invocation, re-deriving a physical pointer the committed root already carried.

The request log of the static fixture (``test/src/static_index.py``) is the
whole mechanism here: "it still resolves" is satisfied just as loudly by a
client that re-fetched everything, so each assertion is a request DELTA around
one command, not an outcome check. The update family is the deliberate
exception — ``ocx update`` / ``ocx index update`` exist to go and look — and it
is asserted positively so the delta assertions cannot pass by a counter that
never moves.

Wire shapes and the ``[registries."ocx.sh"] index`` config surface mirror
``test_index_ocx_sh.py``; the fixture server and the bound-but-not-listening
``dead_endpoint`` idiom are duplicated here rather than imported, per the
suite's DAMP convention for cross-module fixtures.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import socket
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import static_index
from src.helpers import make_package
from src.registry import fetch_manifest_raw
from src.runner import OcxRunner


BINDING = "warmtool"


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


@pytest.fixture()
def dead_endpoint() -> Iterator[str]:
    """A ``127.0.0.1:<port>`` authority that resolves but refuses connections.

    The socket stays BOUND (never listening) for the whole test: a bound socket
    both answers ``ECONNREFUSED`` and reserves the port, so no sibling xdist
    worker can claim it mid-test and turn the refusal into an accept.
    """
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        # Observed, never assumed: the address must actually refuse.
        with pytest.raises(OSError):
            socket.create_connection(("127.0.0.1", port), timeout=1)
        yield f"127.0.0.1:{port}"


def _write_index_config(ocx: OcxRunner, server: static_index.StaticIndexServer) -> None:
    """Points ``[registries."ocx.sh"] index`` at the fixture and trusts the
    physical registry's host (the SSRF escape hatch every fixture here needs,
    since the roots point at the loopback ``registry:2`` instance).
    """
    registry_host = ocx.registry.split(":", 1)[0]
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f'[registries."ocx.sh"]\n'
        f'index = "{server.base_url}"\n'
        f'trusted_hosts = ["{registry_host}", "127.0.0.1"]\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{server.host}"


def _serve_registry_image_index(
    server: static_index.StaticIndexServer,
    repository: str,
    ocx: OcxRunner,
    physical_repo: str,
    tag: str,
) -> str:
    """Serves a root + dispatch object whose ``content`` is the digest the
    REGISTRY served the image index under, and returns the logical identifier.

    Seeded by hand rather than through ``static_index.write_package``, which
    fabricates an image index (placeholder sizes, sorted keys) that no registry
    ever stored. That synthetic digest is unreachable by
    ``GET /v2/<repo>/manifests/<digest>``, so the install's Index-role chain
    blob can never be staged into ``$OCX_HOME/blobs`` and every later install
    re-asks the index for it — a fixture artifact that looks exactly like the
    defect under test. `adr_oci_index_only_dispatch.md`: ``content`` IS the
    registry's own image-index digest.
    """
    served_bytes, served_digest = fetch_manifest_raw(ocx.registry, physical_repo, tag)
    root = {
        "repository": f"oci://{ocx.registry}/{physical_repo}",
        "tags": {tag: {"content": served_digest, "observed": "2026-01-01T00:00:00Z"}},
    }
    root_bytes = json.dumps(root, sort_keys=True, separators=(",", ":")).encode()
    root_path = server.root / "p" / f"{repository}.json"
    root_path.parent.mkdir(parents=True, exist_ok=True)
    root_path.write_bytes(root_bytes)

    index_hex = served_digest.split(":", 1)[1]
    object_path = server.root / "p" / repository / "o" / "sha256" / f"{index_hex}.json"
    object_path.parent.mkdir(parents=True, exist_ok=True)
    object_path.write_bytes(served_bytes)

    static_index.write_catalog(
        server.root, {repository: "sha256:" + hashlib.sha256(root_bytes).hexdigest()}
    )
    return f"ocx.sh/{repository}:{tag}"


@dataclasses.dataclass(slots=True)
class WarmProject:
    """A fully warmed OCX home + project: the package is installed through the
    index, the lock is written, and every command under test has run once.
    """

    ocx: OcxRunner
    server: static_index.StaticIndexServer
    logical_id: str
    project_toml: Path
    cold_requests: int

    def checkpoint(self) -> int:
        return len(self.server.requests)

    def new_paths(self, checkpoint: int) -> list[str]:
        return [record.path for record in self.server.requests[checkpoint:]]


@pytest.fixture()
def warm_project(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> WarmProject:
    """Publishes a package, serves it through the index fixture, installs it
    cold, and runs every command under test once so the local snapshot is
    complete.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)

    _write_index_config(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    logical_id = _serve_registry_image_index(index_server, repository, ocx, pkg.repo, pkg.tag)

    # Cold: the index is contacted here, and only here.
    ocx.plain("package", "install", logical_id)
    cold_requests = len(index_server.requests)

    project = tmp_path / "proj"
    project.mkdir()
    project_toml = project / "ocx.toml"
    project_toml.write_text(f'[tools]\n{BINDING} = "{logical_id}"\n')

    warm = WarmProject(
        ocx=ocx,
        server=index_server,
        logical_id=logical_id,
        project_toml=project_toml,
        cold_requests=cold_requests,
    )
    # One warm-up pass so no command in the measured sweep is a first run.
    for args in _warm_commands(warm, group="warmup"):
        ocx.plain(*args)
    return warm


def _warm_commands(warm: WarmProject, *, group: str) -> list[tuple[str, ...]]:
    """Every command whose resolution must be answered from the local snapshot,
    in an order where each one's prerequisites are already satisfied.

    ``ocx add`` refuses a binding name that already exists, so each pass adds
    the same (already snapshotted) identifier into its own group — the resolve
    it performs is identical, and the measured pass stays a re-run rather than
    a first run of anything else.
    """
    project = ("--project", str(warm.project_toml))
    return [
        ("package", "install", warm.logical_id),
        ("package", "exec", warm.logical_id, "--", "hello"),
        ("package", "which", warm.logical_id),
        (*project, "add", "--group", group, warm.logical_id),
        (*project, "lock"),
        (*project, "pull"),
        (*project, "exec", "--", "hello"),
        (*project, "env"),
        ("index", "list", warm.logical_id),
    ]


def test_warm_commands_never_contact_the_index_source(warm_project: WarmProject) -> None:
    """The headline contract: on a warm home every identifier-resolving command
    lands ZERO requests on the index site.

    Asserted per command with the offending paths named, because a single
    aggregate delta would report "9 requests" without saying which verb spent
    them — and the pre-fix defect spent exactly two on each.
    """
    for args in _warm_commands(warm_project, group="measured"):
        checkpoint = warm_project.checkpoint()
        warm_project.ocx.plain(*args)
        new_paths = warm_project.new_paths(checkpoint)
        assert not new_paths, (
            f"`ocx {' '.join(args)}` contacted the index source on a warm home; "
            f"it must resolve from the committed local snapshot alone. "
            f"Requests: {new_paths}"
        )


def test_a_cold_install_does_contact_the_index_source(warm_project: WarmProject) -> None:
    """The counter can go red: the FIRST install of the same package fetched
    the config declaration and the root document over HTTP.

    Without this the zero-delta sweep above is satisfied by a fixture nothing
    ever reaches — a mistyped `index` URL would pass it.
    """
    assert warm_project.cold_requests > 0, (
        "the cold install must have contacted the index fixture; a zero here "
        "means the warm assertions measure a source that was never wired up"
    )


def test_the_update_family_still_contacts_the_index_source_when_warm(
    warm_project: WarmProject,
) -> None:
    """``ocx update`` and ``ocx index update`` exist to go and look: both must
    still reach the source on a fully warm home.

    This is the exception that keeps the rule falsifiable — a client that
    simply never contacted the index would pass every assertion above and fail
    here.
    """
    for args in (
        ("--project", str(warm_project.project_toml), "update"),
        ("index", "update", warm_project.logical_id),
    ):
        checkpoint = warm_project.checkpoint()
        warm_project.ocx.plain(*args)
        assert warm_project.new_paths(checkpoint), (
            f"`ocx {' '.join(args)}` must re-ask the index source — it is the "
            f"verb whose whole purpose is to observe upstream movement"
        )


def test_warm_exec_and_run_succeed_with_the_index_and_the_registry_dead(
    warm_project: WarmProject,
    dead_endpoint: str,
) -> None:
    """The user-facing property the request deltas stand for: the second
    invocation is offline in effect, without ``--offline``.

    Both remotes are substituted with an endpoint that refuses every
    connection — the index base URL and, via a registry-role ``[mirrors]``
    entry, the physical registry the root points at. A command that still needs
    either one cannot pass by luck here: it fails on a refused connection.
    """
    ocx = warm_project.ocx
    registry_host = ocx.registry.split(":", 1)[0]
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f'[registries."ocx.sh"]\n'
        f'index = "http://{dead_endpoint}"\n'
        f'trusted_hosts = ["{registry_host}", "127.0.0.1"]\n'
        f'\n[mirrors."{ocx.registry}"]\n'
        f'registry = "http://{dead_endpoint}"\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{dead_endpoint}"

    exec_result = ocx.plain("package", "exec", warm_project.logical_id, "--", "hello", check=False)
    assert exec_result.returncode == 0, (
        f"a warm `package exec` must not need either remote: rc={exec_result.returncode}\n"
        f"stderr: {exec_result.stderr.strip()}"
    )

    run_result = ocx.plain(
        "--project", str(warm_project.project_toml), "exec", "--", "hello", check=False
    )
    assert run_result.returncode == 0, (
        f"a warm `ocx run` must not need either remote: rc={run_result.returncode}\n"
        f"stderr: {run_result.stderr.strip()}"
    )

