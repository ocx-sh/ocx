# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the offline-after-pull contract (issue #424).

Full offline capability after an initial pull is a **correctness**
requirement, not a latency optimisation: a machine that pulls on Monday and
loses its network on Tuesday must still run every tool its ``ocx.lock`` pins.
The user typed ``cmake``, not ``ocx pull`` — there is no interactive fallback
on that path.

What #424 actually measured
---------------------------
A rendered trampoline re-enters as ``ocx --project '<root>' exec --
<name> "$@"``. That resolve reached ``resolve_transport_pinned``
(``crates/ocx_lib/src/package_manager/tasks/resolve.rs``), which asks the
index chain for the *physical* transport location of a logical reference.
With no committed root in ``$OCX_HOME/index`` the chain fell through to the
source and issued ``GET https://index.ocx.sh/p/<ns>/<pkg>.json`` — **once per
locked tool, on every invocation**, for a pointer that only ever routes a
download this invocation was never going to make.

Why the fixture deletes ``$OCX_HOME/index``
-------------------------------------------
That deletion IS the reported condition ("an ``$OCX_HOME`` that holds no index
copy"), and it is not exotic: a digest-addressed resolve never grows the local
root (``ChainedIndex``'s ``grow_root`` is false for an ``AbsentDispatch``), so
a machine that only ever runs ``pull`` and ``exec`` against a committed lock —
a fresh clone, a CI checkout, a restored store cache — is in exactly this state
forever. Only the tag-addressed verbs (``add``/``lock``/``index update``)
commit a root, which is why a developer box that ran them looks warm and hides
the defect.

Latency is deliberately not asserted
------------------------------------
A timing assertion reds on a slow runner for the wrong reason. Two mechanisms
stand in for it, and both are falsifiable:

* the request log of the static index fixture (``test/src/static_index.py``) —
  a **delta of zero** around the trampoline invocation, with the offending
  paths named when it is not;
* both remotes replaced by an endpoint that refuses every connection, so a
  command that still needs either one fails rather than passes slowly.

The two controls that keep the above from passing vacuously are their own
tests: :func:`test_the_cold_pull_did_contact_the_index_source` (the fixture is
wired at all) and
:func:`test_a_store_with_nothing_local_still_pulls_on_first_use` (the fix did
not buy offline by breaking the cold start).

The fixture wiring and the ``dead_endpoint`` idiom are duplicated from
``test_warm_resolve_no_network.py`` rather than imported, per the suite's DAMP
convention for cross-module fixtures.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import shutil
import socket
import subprocess
import tomllib
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import static_index
from src.helpers import make_package, write_ocx_toml
from src.registry import fetch_manifest_raw
from src.runner import OcxRunner, current_platform
from src.toolchain_fixtures import resolved_toolchain_home, run_in, shell_bin

EXIT_SUCCESS = 0

#: The ``[tools]`` key, the exposed binary name and the repository are three
#: disjoint spellings on purpose — an assertion about the trampoline must not
#: be satisfiable by any of the other two.
BINDING = "offlinekey"
BINARY = "offlinebin"

#: Every subprocess here is bounded: the pre-fix failure mode against a
#: black-holed endpoint is a *hang*, not an exit.
TIMEOUT = 90


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
    since the root points at the loopback registry instance).
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
) -> tuple[str, str]:
    """Serves a root + dispatch object whose ``content`` is the digest the
    REGISTRY served the image index under, and returns the logical identifier
    paired with that image-index digest.

    Seeded by hand rather than through ``static_index.write_package``, which
    fabricates an image index whose synthetic digest no registry ever stored:
    that digest is unreachable by ``GET /v2/<repo>/manifests/<digest>``, so the
    install's Index-role chain blob could never be staged into
    ``$OCX_HOME/blobs`` and every later resolve would re-ask the index for it —
    a fixture artefact that looks exactly like the defect under test.
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
    return f"ocx.sh/{repository}:{tag}", served_digest


def _served_project(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
):
    """Publish a package, serve it through the index fixture under a logical
    ``ocx.sh/...`` name, and write the project that declares it.

    Returns ``(pkg, directory, repository)``. The indirection is the point: the
    logical name is what ``ocx.toml`` carries and the physical registry is what
    the index root points at, so a resolve that needs the physical location has
    to get it from somewhere.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, cascade=False, index=False, bins=[BINARY]
    )

    _write_index_config(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    logical_id, image_index_digest = _serve_registry_image_index(
        index_server, repository, ocx, pkg.repo, pkg.tag
    )

    directory = tmp_path / "proj"
    directory.mkdir()
    write_ocx_toml(
        directory, f'activate = "bin"\n\n[tools]\n{BINDING} = "{logical_id}"\n'
    )
    return pkg, directory, repository, image_index_digest


def _local_index_root(ocx: OcxRunner) -> Path:
    """``$OCX_HOME/index`` — asserted to exist by every caller that removes it,
    so a layout change reds rather than silently turning a cold-index case into
    a warm one."""
    return Path(ocx.env["OCX_HOME"]) / "index"


@dataclasses.dataclass(slots=True)
class PulledToolchain:
    """A project whose toolchain is locked, pulled and rendered, in an
    ``$OCX_HOME`` that holds no index copy."""

    ocx: OcxRunner
    server: static_index.StaticIndexServer
    directory: Path
    trampoline: Path
    marker: str
    cold_requests: int
    repository: str
    #: The digest the REGISTRY served the image index under — what `ocx.lock`
    #: must NOT record, since the store is keyed on the platform leaf.
    image_index_digest: str

    def checkpoint(self) -> int:
        return len(self.server.requests)

    def new_paths(self, checkpoint: int) -> list[str]:
        return [record.path for record in self.server.requests[checkpoint:]]


@pytest.fixture()
def pulled_toolchain(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> PulledToolchain:
    pkg, directory, repository, image_index_digest = _served_project(
        ocx, unique_repo, tmp_path, index_server
    )

    for verb in ("lock", "pull"):
        result = run_in(ocx, directory, verb)
        assert result.returncode == EXIT_SUCCESS, f"ocx {verb} failed:\n{result.stderr}"

    cold_requests = len(index_server.requests)

    home = resolved_toolchain_home(ocx, directory)
    trampoline = shell_bin(home) / BINARY
    assert trampoline.is_file(), (
        f"the pull must render a trampoline at {trampoline}; the tree holds "
        f"{sorted(p.name for p in shell_bin(home).glob('*'))}"
    )

    # The #424 condition, applied explicitly. Asserted before it is removed:
    # a layout change that moved the local index elsewhere would otherwise turn
    # every case below into a test of a warm home that never needed the network.
    local_index = _local_index_root(ocx)
    assert local_index.is_dir(), (
        f"the local index copy must exist at {local_index} before the fixture "
        f"removes it, or these cases assert nothing"
    )
    shutil.rmtree(local_index)

    return PulledToolchain(
        ocx=ocx,
        server=index_server,
        directory=directory,
        trampoline=trampoline,
        marker=pkg.marker,
        cold_requests=cold_requests,
        repository=repository,
        image_index_digest=image_index_digest,
    )


def _run_trampoline(
    pulled: PulledToolchain | ColdIndexProject,
    cwd: Path,
    env_extra: dict[str, str] | None = None,
):
    """Invoke the rendered trampoline as a user would — by path, from an
    unrelated directory, with no ``ocx`` argument of its own.

    Not ``run_in``: the file under test *is* the trampoline, and the whole
    point is that it decides its own project from the root baked into its body.
    """
    env = dict(pulled.ocx.env)
    if env_extra:
        env.update(env_extra)
    return subprocess.run(
        [str(pulled.trampoline)],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=TIMEOUT,
    )


def test_the_cold_pull_did_contact_the_index_source(
    pulled_toolchain: PulledToolchain,
) -> None:
    """Non-vacuity: the fixture's index is reachable-if-tried.

    Without this, "the trampoline landed zero requests" is satisfied just as
    loudly by a mistyped ``index`` URL that nothing ever reached, and "it ran
    with the endpoint dead" by a fixture that never needed an endpoint.
    """
    assert pulled_toolchain.cold_requests > 0, (
        "the cold lock+pull must have contacted the index fixture; a zero here "
        "means every offline assertion in this module measures a source that "
        "was never wired up"
    )


def test_a_pulled_toolchain_runs_through_its_trampoline_with_both_remotes_dead(
    pulled_toolchain: PulledToolchain,
    dead_endpoint: str,
    tmp_path: Path,
) -> None:
    """The mandate itself: pulled on Monday, no network on Tuesday, still runs.

    Both remotes are substituted with an endpoint that refuses every
    connection — the index base URL, and via a registry-role ``[mirrors]``
    entry the physical registry the root points at. A trampoline that still
    needs either one cannot pass here by luck: it fails on a refused
    connection, or hangs into :data:`TIMEOUT`.
    """
    ocx = pulled_toolchain.ocx
    registry_host = ocx.registry.split(":", 1)[0]
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f'[registries."ocx.sh"]\n'
        f'index = "http://{dead_endpoint}"\n'
        f'trusted_hosts = ["{registry_host}", "127.0.0.1"]\n'
        f'\n[mirrors."{ocx.registry}"]\n'
        f'registry = "http://{dead_endpoint}"\n'
    )

    result = _run_trampoline(
        pulled_toolchain,
        tmp_path,
        {"OCX_INSECURE_REGISTRIES": f"{ocx.registry},{dead_endpoint}"},
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"a pulled tool must run through its trampoline with no network at all; "
        f"rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert pulled_toolchain.marker in result.stdout, (
        f"the trampoline must run the tool's own binary, not merely exit 0; "
        f"got {result.stdout!r}"
    )


def test_the_trampoline_lands_zero_requests_on_a_live_index(
    pulled_toolchain: PulledToolchain,
    tmp_path: Path,
) -> None:
    """The same property, measured instead of blocked — and the case that
    names *which* request was spent when it regresses.

    The index fixture stays up, so a client that dials succeeds and this is the
    only signal that it did. The pre-fix delta was one ``GET /config.json``
    plus one ``GET /p/<ns>/<pkg>.json`` per locked tool.
    """
    checkpoint = pulled_toolchain.checkpoint()

    result = _run_trampoline(pulled_toolchain, tmp_path)

    assert result.returncode == EXIT_SUCCESS, (
        f"precondition: the trampoline must run; rc={result.returncode}\n{result.stderr}"
    )
    assert pulled_toolchain.marker in result.stdout, result.stdout
    assert not pulled_toolchain.new_paths(checkpoint), (
        f"a trampoline whose every digest is pinned in the lock and present in "
        f"the store must contact the index source zero times; it requested "
        f"{pulled_toolchain.new_paths(checkpoint)}"
    )


def test_a_store_with_nothing_local_still_pulls_on_first_use(
    pulled_toolchain: PulledToolchain,
    tmp_path: Path,
) -> None:
    """The cold-start control: the fix must not buy offline by never dialling.

    Everything local is removed — the package tree, the blob CAS and the
    extracted layers — so the only way this invocation can produce the tool's
    marker is to resolve the lock's pinned digest through the index (the very
    physical-pointer lookup the warm path now skips) and pull it. A change that
    made the trampoline unconditionally offline reds here, on a path with no
    interactive fallback.
    """
    home = Path(pulled_toolchain.ocx.env["OCX_HOME"])
    for directory in ("packages", "blobs", "layers"):
        shutil.rmtree(home / directory, ignore_errors=True)

    checkpoint = pulled_toolchain.checkpoint()

    result = _run_trampoline(pulled_toolchain, tmp_path)

    assert result.returncode == EXIT_SUCCESS, (
        f"first use through a trampoline must still pull; rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert pulled_toolchain.marker in result.stdout, result.stdout
    assert pulled_toolchain.new_paths(checkpoint), (
        "a cold store must have re-asked the index source; a zero delta here "
        "means the pull was answered from something this test believed it had "
        "deleted"
    )


# ---------------------------------------------------------------------------
# The other half: what a digest pull teaches the local index
# ---------------------------------------------------------------------------
#
# The store probe above makes a warm invocation dial nothing. It does not make
# the machine *know* anything: the routing pointer that says where an
# indirected package's bytes live is learned from the source and, before this,
# thrown away every time. `resolution.registries` in an execution record is
# derived from it, so losing it silently is a provenance regression, and every
# path the store probe does not cover keeps re-asking.
#
# So a non-`--frozen` resolve that had to ask a source records the answer.
# Under `--frozen` it does not: that flag is a promise the invocation changes
# nothing, and buying offline capability by quietly mutating the index under it
# would trade a stated guarantee for an unstated one.


@dataclasses.dataclass(slots=True)
class ColdIndexProject:
    """A fresh clone's shape: a committed lock, a populated store, and an
    ``$OCX_HOME`` whose index copy was empty when ``ocx pull`` ran."""

    ocx: OcxRunner
    server: static_index.StaticIndexServer
    directory: Path
    trampoline: Path
    marker: str
    root_path: Path
    physical_repository: str


def _cold_index_pull(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    *,
    pull_args: tuple[str, ...] = ("pull",),
) -> ColdIndexProject:
    """Lock, wipe the index copy, then pull — the digest-addressed pull into an
    empty index that a fresh clone of a locked repository performs.

    The lock step is what makes the pull digest-addressed; the wipe is what
    makes the index empty. Doing them in this order rather than seeding a
    hand-written ``ocx.lock`` keeps the lock the real one ``ocx`` writes.
    """
    pkg, directory, repository, _index_digest = _served_project(
        ocx, unique_repo, tmp_path, index_server
    )

    locked = run_in(ocx, directory, "lock")
    assert locked.returncode == EXIT_SUCCESS, f"ocx lock failed:\n{locked.stderr}"

    local_index = _local_index_root(ocx)
    assert local_index.is_dir(), (
        f"the tag-addressed lock must have grown a local index at {local_index}; "
        f"without that precondition the wipe below is a no-op and the pull is "
        f"not the cold-index case this fixture is for"
    )
    shutil.rmtree(local_index)

    pulled = run_in(ocx, directory, *pull_args)
    assert pulled.returncode == EXIT_SUCCESS, (
        f"ocx {' '.join(pull_args)} failed:\n{pulled.stderr}"
    )

    home = resolved_toolchain_home(ocx, directory)
    return ColdIndexProject(
        ocx=ocx,
        server=index_server,
        directory=directory,
        trampoline=shell_bin(home) / BINARY,
        marker=pkg.marker,
        root_path=local_index / "ocx.sh" / "p" / f"{repository}.json",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
    )


@pytest.fixture()
def cold_index_pull(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> ColdIndexProject:
    return _cold_index_pull(ocx, unique_repo, tmp_path, index_server)


def test_a_digest_pull_into_an_empty_index_records_the_routing_pointer(
    cold_index_pull: ColdIndexProject,
) -> None:
    """The pull learned where the content lives; it must write that down.

    Parsed, not grepped: the assertion is on the ``repository`` field a later
    resolve actually reads, so a document that happens to contain the host
    string somewhere else does not satisfy it.

    ``tags`` must be empty. That is what distinguishes recording a routing
    pointer from adopting a package: a digest pull names no tag, and writing
    one would move a pin the user never asked for.
    """
    assert cold_index_pull.root_path.is_file(), (
        f"a non-frozen digest pull must record the routing pointer at "
        f"{cold_index_pull.root_path}; the index tree holds "
        f"{sorted(str(p) for p in _local_index_root(cold_index_pull.ocx).rglob('*.json'))}"
    )
    root = json.loads(cold_index_pull.root_path.read_text())
    assert root["repository"] == cold_index_pull.physical_repository, (
        f"the recorded pointer must name the physical location the index served; "
        f"got {root.get('repository')!r}"
    )
    assert root.get("tags") == {}, (
        f"recording a routing pointer must adopt no tag pointers — that is "
        f"`ocx index update <pkg>`'s job; got {root.get('tags')!r}"
    )


def test_a_warm_execution_record_still_names_the_transport_registry(
    cold_index_pull: ColdIndexProject,
    tmp_path: Path,
) -> None:
    """The provenance the store probe would otherwise cost.

    ``resolution.registries`` names the content registry an auditor needs to
    reach the same bytes again. A warm invocation fetches nothing, so it can
    only report that host if the machine wrote it down — which is what the
    routing pointer is for.
    """
    sink = tmp_path / "records"
    sink.mkdir()

    result = _run_trampoline(cold_index_pull, tmp_path, {"OCX_RECORDS_DIR": str(sink)})

    assert result.returncode == EXIT_SUCCESS, (
        f"the trampoline must run; rc={result.returncode}\n{result.stderr}"
    )
    records = sorted(sink.rglob("*.json"))
    assert len(records) == 1, f"expected exactly one record in {sink}; got {records}"
    resolution = json.loads(records[0].read_text())["resolution"]
    assert resolution.get("registries") == [cold_index_pull.ocx.registry], (
        f"a warm frame must still name the content registry it would have "
        f"fetched from; got {resolution.get('registries')!r}"
    )


def test_a_frozen_pull_records_no_routing_pointer(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """``--frozen`` says the invocation changes nothing, and that wins.

    The consequence is deliberate and is the reason this case exists: a frozen
    invocation against a cold index may still dial. Someone will eventually be
    tempted to "improve" the fix by writing under frozen too, and this is what
    stops them.

    The pull itself must still succeed — ``--frozen`` freezes tag resolution,
    not a digest the lock already pins — so a red here cannot be mistaken for
    the flag simply rejecting the command.
    """
    frozen = _cold_index_pull(
        ocx, unique_repo, tmp_path, index_server, pull_args=("--frozen", "pull")
    )

    assert not frozen.root_path.exists(), (
        f"`--frozen` must write nothing into the local index; it wrote "
        f"{frozen.root_path.read_text() if frozen.root_path.is_file() else frozen.root_path}"
    )


# ---------------------------------------------------------------------------
# The case the store probe does NOT cover, and why it cannot arrive
# ---------------------------------------------------------------------------


def test_the_lock_pins_platform_leaves_so_an_image_index_digest_never_arrives(
    pulled_toolchain: PulledToolchain,
    dead_endpoint: str,
    tmp_path: Path,
) -> None:
    """`find`'s store probe is keyed on the package store, which is keyed on the
    **platform leaf** digest. An *image-index* digest would therefore miss it and
    fall through to a resolve — and a resolve dials.

    That case cannot arrive from a lock. `ocx lock` resolves the platform
    fan-out at lock time and records one digest per platform under
    ``[tool.platforms]``, so the identifier the resolve path receives is always
    a leaf. The fixture serves a real image index and this asserts the lock did
    **not** record its digest, which is the discriminating half: if `ocx.lock`
    ever recorded the index digest instead, offline capability would silently
    stop covering every multi-platform tool — the normal shape for anything
    shipped across operating systems — and nothing else in this module would
    notice.

    Tested rather than dismissed for exactly that reason: what makes the case
    impossible is a *lock format* property, which a future change could alter
    without anyone connecting it to offline capability.
    """
    lock = tomllib.loads((pulled_toolchain.directory / "ocx.lock").read_text())
    platforms = lock["tool"][0]["platforms"]
    assert platforms, (
        f"the lock must record per-platform digests; got {lock['tool'][0]}"
    )

    host = current_platform()
    recorded = [digest for key, digest in platforms.items() if key.startswith(host)]
    assert len(recorded) == 1, (
        f"exactly one lock entry must describe this host ({host}); got {platforms}"
    )
    leaf = recorded[0]

    assert leaf != pulled_toolchain.image_index_digest, (
        f"the lock recorded the image-index digest {leaf} rather than a platform "
        f"leaf — the store is keyed on the leaf, so every multi-platform tool "
        f"would miss the store probe and dial on every invocation"
    )

    # And the leaf is the digest the store actually answers: resolvable with
    # both remotes dead, which an image-index digest would not be.
    ocx = pulled_toolchain.ocx
    registry_host = ocx.registry.split(":", 1)[0]
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f'[registries."ocx.sh"]\n'
        f'index = "http://{dead_endpoint}"\n'
        f'trusted_hosts = ["{registry_host}", "127.0.0.1"]\n'
        f'\n[mirrors."{ocx.registry}"]\n'
        f'registry = "http://{dead_endpoint}"\n'
    )
    result = ocx.run(
        "package",
        "exec",
        f"ocx.sh/{pulled_toolchain.repository}@{leaf}",
        "--",
        BINARY,
        format=None,
        check=False,
        env_overrides={"OCX_INSECURE_REGISTRIES": f"{ocx.registry},{dead_endpoint}"},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"the lock's recorded digest must resolve from the store with no network; "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )
    assert pulled_toolchain.marker in result.stdout, result.stdout
