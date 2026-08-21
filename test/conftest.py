"""Shared fixtures and hooks for all test suites (tests/ and recordings/)."""
from __future__ import annotations

import dataclasses
import importlib.util
import os
import stat
import sys
import textwrap
import threading
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT, start_registry
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Session hooks
# ---------------------------------------------------------------------------

# Default addresses for the two registry:2 services, derived from the same
# port knobs `docker-compose.yml` binds with. A machine that already runs
# something on 5000 (another checkout, an unrelated project) exports
# OCX_TEST_REGISTRY_PORT once and compose, the taskfile and these defaults all
# move together. An explicit REGISTRY / MIRROR_REGISTRY still wins over both.
_DEFAULT_REGISTRY = f"localhost:{os.environ.get('OCX_TEST_REGISTRY_PORT', '5000')}"
_DEFAULT_MIRROR_REGISTRY = f"localhost:{os.environ.get('OCX_TEST_MIRROR_PORT', '5001')}"
_DEFAULT_TARGET_REGISTRY = f"localhost:{os.environ.get('OCX_TEST_TARGET_PORT', '5003')}"


def _wait_for_reachable(
    is_reachable: Callable[[], bool],
    *,
    attempts: int = 10,
    delay_seconds: float = 0.5,
    sleep: Callable[[float], None] = time.sleep,
) -> bool:
    """Polls ``is_reachable`` up to ``attempts`` times, sleeping between tries.

    Returns ``True`` the moment a call succeeds, ``False`` once every attempt
    is exhausted. ``sleep`` is injectable so a test can prove the polling
    shape (attempt count, no sleep after the last try) without a real clock.
    """
    for attempt in range(attempts):
        if is_reachable():
            return True
        if attempt < attempts - 1:
            sleep(delay_seconds)
    return False


def pytest_sessionstart(session: pytest.Session) -> None:
    """Start the registry (and secondary registries) once before xdist workers spawn.

    Registry-independent opt-out (``OCX_TESTS_NO_REGISTRY=1``): the Windows
    native-shim acceptance suite (``tests/test_windows_shim.py``) builds a
    fake ``pkg_root`` on disk and never touches a registry — and the
    ``registry:2`` Docker Compose fixture does not start on ``windows-latest``
    (system_design §8). Selecting only that module on a runner without Docker
    sets this flag so ``pytest_sessionstart`` does not hard-fail trying to
    ``docker compose up`` a registry no collected test needs.

    The mirror registry (``localhost:5001``) and the promotion target
    (``localhost:5003``) are started under the same opt-out guard. Both are
    declared in the same docker-compose.yml as the primary registry, but
    ``start_registry`` returns early when the primary is already warm, so it
    cannot be relied on to have created them: each secondary is brought up
    here in its own right, then given a bounded retry for the beat its port
    takes to bind. A secondary that is still unreachable after both is a
    hard failure — Docker is available, so an absent service is a broken
    environment, and skipping instead would empty whole suites (every test
    in ``test_package_copy.py`` depends on ``target-registry``) while the
    run still exits 0.
    """
    if os.environ.get("PYTEST_XDIST_WORKER") is not None:
        return
    if os.environ.get("OCX_TESTS_NO_REGISTRY") == "1":
        return
    registry = os.environ.get("REGISTRY", _DEFAULT_REGISTRY)
    start_registry(registry)

    from src.helpers import registry_is_reachable  # noqa: PLC0415

    from src.helpers import compose_up  # noqa: PLC0415

    secondaries = (
        ("mirror-registry", os.environ.get("MIRROR_REGISTRY", _DEFAULT_MIRROR_REGISTRY)),
        ("target-registry", os.environ.get("TARGET_REGISTRY", _DEFAULT_TARGET_REGISTRY)),
    )
    for service, address in secondaries:
        if registry_is_reachable(address):
            continue
        # A warm primary makes `start_registry` a no-op, so a sibling service
        # that was never created stays absent — the retry alone would just
        # burn its budget against a container that does not exist. Issue the
        # compose up here rather than assuming the primary's start did it.
        compose_up()
        if _wait_for_reachable(lambda address=address: registry_is_reachable(address)):
            continue
        # Hard failure, not a skip. Docker is available (the opt-out above did
        # not fire), so an absent service is a broken environment, not a
        # legitimately reduced one — and a skip here silently empties whole
        # suites while the run still exits 0.
        raise RuntimeError(
            f"{service} at {address} did not become reachable after "
            f"`docker compose up -d` and a bounded retry; the suites that "
            f"need it would otherwise skip and the run would still pass"
        )


# ---------------------------------------------------------------------------
# Session-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def registry() -> str:
    addr = os.environ.get("REGISTRY", _DEFAULT_REGISTRY)
    start_registry(addr)
    return addr


@pytest.fixture(scope="session")
def mirror_registry() -> str:
    """Session-scoped mirror registry (localhost:5001, the second registry:2).

    Parallel to the ``registry`` fixture but targets the mirror service.
    Returns the registry address on success or skips the test if the mirror
    registry is not reachable (single-registry environment, no Docker, or
    Windows runner that sets ``OCX_TESTS_NO_REGISTRY=1``).

    Tests in ``test_oci_registry_mirror.py`` declare this as a fixture
    dependency so they are automatically skipped when the mirror is absent.
    """
    if os.environ.get("OCX_TESTS_NO_REGISTRY") == "1":
        pytest.skip("OCX_TESTS_NO_REGISTRY=1: mirror registry not started")

    addr = os.environ.get("MIRROR_REGISTRY", _DEFAULT_MIRROR_REGISTRY)

    from src.helpers import registry_is_reachable  # noqa: PLC0415
    if not registry_is_reachable(addr):
        pytest.skip(
            f"mirror registry at {addr} is not reachable; "
            "test_oci_registry_mirror.py requires the docker-compose 'mirror-registry' service"
        )
    return addr


@pytest.fixture(scope="session")
def target_registry() -> str:
    """Session-scoped promotion target (the second zot, localhost:5003).

    A cross-registry ``ocx package copy`` needs a target that implements the OCI
    1.1 Referrers API, so it cannot be ``mirror_registry`` — that one is
    registry:2 and deliberately has none. Consumed by
    ``tests/test_package_copy.py``.

    Skips when the service is unreachable, on the same terms as
    ``mirror_registry``: a single-registry environment, no Docker, or a Windows
    runner that sets ``OCX_TESTS_NO_REGISTRY=1``.
    """
    if os.environ.get("OCX_TESTS_NO_REGISTRY") == "1":
        pytest.skip("OCX_TESTS_NO_REGISTRY=1: target registry not started")

    addr = os.environ.get("TARGET_REGISTRY", _DEFAULT_TARGET_REGISTRY)

    from src.helpers import registry_is_reachable  # noqa: PLC0415
    if not registry_is_reachable(addr):
        pytest.skip(
            f"target registry at {addr} is not reachable; "
            "test_package_copy.py requires the docker-compose 'target-registry' service"
        )
    return addr


@pytest.fixture(scope="session")
def legacy_registry() -> str:
    """Session-scoped referrers-NEGATIVE fixture (registry:2, localhost:5001).

    A real OCI Distribution v2 registry that does NOT implement the OCI 1.1
    Referrers API: ``GET /v2/<name>/referrers/<digest>`` returns 404. Consumed
    by ``test_referrers_capability.py`` (#106) to assert the clean
    ``ReferrersUnsupported`` / exit-84 path against a genuine v2 registry, and
    by ``test_referrers_smoke.py`` to prove the harness carries a real
    referrers-unsupported registry.

    Backed by the SAME docker-compose ``mirror-registry`` service as
    ``mirror_registry`` — the compose file keeps exactly one ``registry:2``
    instance, which serves both the mirror-test and referrers-negative roles.
    Skips (like ``mirror_registry``) when the service is unreachable: a
    single-registry environment, no Docker, or a Windows runner that sets
    ``OCX_TESTS_NO_REGISTRY=1``.
    """
    if os.environ.get("OCX_TESTS_NO_REGISTRY") == "1":
        pytest.skip("OCX_TESTS_NO_REGISTRY=1: legacy (registry:2) fixture not started")

    addr = os.environ.get("LEGACY_REGISTRY", _DEFAULT_MIRROR_REGISTRY)

    from src.helpers import registry_is_reachable  # noqa: PLC0415
    if not registry_is_reachable(addr):
        pytest.skip(
            f"legacy registry:2 fixture at {addr} is not reachable; "
            "test_referrers_capability.py requires the docker-compose 'mirror-registry' service"
        )
    return addr


@pytest.fixture(scope="session")
def ocx_binary() -> Path:
    if env_path := os.environ.get("OCX_COMMAND"):
        p = Path(env_path)
    else:
        p = PROJECT_ROOT / "test" / "bin" / "ocx"
        if sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")
    assert p.exists(), f"ocx binary not found at {p}"
    return p


# ---------------------------------------------------------------------------
# Function-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture()
def ocx_home(tmp_path: Path) -> Path:
    home = tmp_path / "ocx-home"
    home.mkdir()
    return home


@pytest.fixture()
def ocx(ocx_binary: Path, ocx_home: Path, registry: str) -> OcxRunner:
    return OcxRunner(ocx_binary, ocx_home, registry)


def _load_fake_forge_module():
    """Loads `test/tests/fake_forge.py` by path.

    `tests/` carries no `__init__.py` (pytest's rootless test-directory
    convention — see every sibling `test_*.py`), so it is not a regular
    importable package from this session-root conftest. Loading by file path
    sidesteps that and any import-mode/collection-order ambiguity.
    """
    module_path = Path(__file__).parent / "tests" / "fake_forge.py"
    spec = importlib.util.spec_from_file_location("fake_forge", module_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture()
def fake_forge():
    """A per-test fake GitHub REST forge (`test/tests/fake_forge.py`).

    Bound to an ephemeral loopback port, zero real network. Point
    `GitHubForge` at it via the `__OCX_TESTING_FORGE_BASE_URL` env override
    (e.g. ``ocx.run(..., env_overrides={"__OCX_TESTING_FORGE_BASE_URL": fake_forge.base_url})``).
    """
    server = _load_fake_forge_module().FakeForge()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def _load_fake_registry_module():
    """Loads `test/tests/fake_registry.py` by path — same reason as
    `_load_fake_forge_module` above (`tests/` is not an importable package)."""
    module_path = Path(__file__).parent / "tests" / "fake_registry.py"
    spec = importlib.util.spec_from_file_location("fake_registry", module_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture()
def html_mirror():
    """A host that answers every manifest request with an HTML portal page
    (`test/tests/fake_registry.py`).

    The shape of ocx-sh/ocx#327: a mirror pointed at a tenant that no longer
    serves the registry. Neither `registry:2` nor `zot` can be made to do this
    — both refuse to serve bytes that are not the manifest they claim to be.

    Bound to an ephemeral loopback port, zero real network. Point OCX at it
    with ``[mirrors] "<upstream>" = "<html_mirror.base_url>"``.
    """
    server = _load_fake_registry_module().HtmlMirror()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


# ---------------------------------------------------------------------------
# Mock docker credential helper
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class MockHelper:
    """A `docker-credential-test` shell-script helper installed under a tempdir.

    Tests prepend ``dir`` to the subprocess PATH and configure ``credsStore`` or
    ``credHelpers`` to point at the suffix ``test``. The helper's stdin is
    captured in the sidecar file.
    """

    path: Path
    dir: Path
    sidecar: Path
    docker_config_dir: Path


@pytest.fixture()
def mock_credential_helper(tmp_path: Path) -> MockHelper:
    """A mock ``docker-credential-test`` helper with parameterizable behavior.

    Default behavior persists stdin to a sidecar JSON file and responds to
    ``get`` by reading the same file. Tests parameterize behavior by editing
    the script body — see ``MockHelper.path``.
    """
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    bin_path = helper_dir / "docker-credential-test"

    # Default: persistent map keyed by server URL in the sidecar.
    script = textwrap.dedent(
        f"""\
        #!/bin/sh
        # Mock docker credential helper for OCX acceptance tests.
        # Default behavior: persist stdin to {sidecar}; emit on get.
        action="$1"
        sidecar="{sidecar}"
        input=$(cat)
        case "$action" in
            store)
                # JSON input on stdin.
                printf '%s' "$input" > "$sidecar"
                ;;
            get)
                if [ -f "$sidecar" ]; then
                    cat "$sidecar"
                else
                    echo 'credentials not found in native keychain'
                    exit 1
                fi
                ;;
            erase)
                rm -f "$sidecar"
                ;;
            list)
                echo '{{}}'
                ;;
            *)
                echo "unknown action: $action" >&2
                exit 2
                ;;
        esac
        """
    )
    bin_path.write_text(script)
    bin_path.chmod(bin_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()

    return MockHelper(
        path=bin_path,
        dir=helper_dir,
        sidecar=sidecar,
        docker_config_dir=docker_config_dir,
    )
