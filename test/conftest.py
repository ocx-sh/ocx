"""Shared fixtures and hooks for all test suites (tests/ and recordings/)."""

from __future__ import annotations

import contextlib
import dataclasses
import faulthandler
import importlib.util
import os
import shutil
import stat
import sys
import textwrap
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from src import forward_proxy as forward_proxy_mod
from src import zot
from src.helpers import (
    PROJECT_ROOT,
    compose_up_service,
    registry_is_reachable,
    skip_without_docker,
)
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Session hooks
# ---------------------------------------------------------------------------

# The registry:2 mirror (compose service `mirror-registry`) keeps a fixed port:
# OCX_TEST_MIRROR_PORT moves it, as it moves `docker-compose.yml`. The zot
# registries have no default: their ports exist only after `pytest_configure`.
_DEFAULT_MIRROR_REGISTRY = f"localhost:{os.environ.get('OCX_TEST_MIRROR_PORT', '5001')}"
_ZOT = pytest.StashKey[zot.Stack]()
_TEST_ROOT = Path(__file__).resolve().parent


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


def _wants_zot(config: pytest.Config) -> bool:
    """Whether this process starts the session's zot registries.

    Not the xdist workers (they read the controller's ports from the
    environment), not a run that executes no test (`--collect-only`, `--help`),
    not a run that opted out, and not the lint tier, which may reach no registry.
    """
    if hasattr(config, "workerinput") or os.environ.get("OCX_TESTS_NO_REGISTRY") == "1":
        return False
    if (
        config.getoption("collectonly")
        or config.getoption("help")
        or config.getoption("version")
    ):
        return False
    invoked_from = Path(config.invocation_params.dir)
    targets = [
        (invoked_from / str(arg).split("::")[0]).resolve() for arg in config.args
    ]
    # A string compare on the first segment, not a joined path: the tag guard reads
    # a `test/<dir>` join as an input every acceptance module would have to declare.
    in_lint = [
        _TEST_ROOT in t.parents and t.relative_to(_TEST_ROOT).parts[0] == "lint"
        for t in targets
    ]
    return not (targets and all(in_lint))


def pytest_configure(config: pytest.Config) -> None:
    """Each xdist worker gets its own session, so nothing a test spawns can
    reach the terminal `task verify` runs in -- no tcsetpgrp() steal, no
    SIGTTIN. The controller keeps it, so Ctrl-C still works.

    The controller starts the session's zot registries here, before xdist spawns
    a worker, and exports their ports: workers inherit the environment and never
    start one (`src/zot.py`).
    """
    if hasattr(config, "workerinput") and hasattr(
        os, "setsid"
    ):  # no sessions on Windows
        with contextlib.suppress(OSError):
            os.setsid()
    if not _wants_zot(config):
        return
    try:
        stack = zot.start()
    except zot.ZotError as error:
        raise pytest.UsageError(str(error)) from None
    config.stash[_ZOT] = stack
    os.environ.update(stack.env())


def pytest_unconfigure(config: pytest.Config) -> None:
    if (stack := config.stash.get(_ZOT, None)) is not None:
        stack.stop()


@pytest.hookimpl(wrapper=True)
def pytest_runtest_protocol(item: pytest.Item, nextitem: pytest.Item | None) -> object:
    """A test still running after ``OCX_TEST_DEADLINE_SECONDS`` (default 600) dumps every
    thread's stack to ``<basetemp>/deadline-<pid>.log`` and kills its process, which xdist
    reports as a crashed worker naming the test. Without it a spin, such as a walk through
    the store's junction cycle on Windows, holds a CI shard until the six-hour job limit.
    """
    deadline = float(os.environ.get("OCX_TEST_DEADLINE_SECONDS", "600"))
    log = item.config._tmp_path_factory.getbasetemp() / f"deadline-{os.getpid()}.log"
    with log.open("w", encoding="utf-8") as fh:
        faulthandler.dump_traceback_later(deadline, exit=True, file=fh)
        try:
            return (yield)
        finally:
            faulthandler.cancel_dump_traceback_later()


# ---------------------------------------------------------------------------
# Session-scoped fixtures
# ---------------------------------------------------------------------------


def _zot_address(instance: str) -> str:
    if os.environ.get("OCX_TESTS_NO_REGISTRY") == "1":
        pytest.skip(f"OCX_TESTS_NO_REGISTRY=1: {instance} registry not started")
    port = os.environ.get(f"OCX_TEST_{instance.upper()}_PORT")
    assert port, (
        f"no zot `{instance}` port in the environment: test/conftest.py::pytest_configure did not start one"
    )
    return f"localhost:{port}"


def _mirror_address(address: str, fixture: str) -> str:
    """The compose ``mirror-registry`` (registry:2), started on first use.

    Skips only on a job that declares ``OCX_TEST_NO_DOCKER=1``. Without it a
    missing ``docker`` CLI, a dead daemon and a service that never answers all
    fail: a skip there would empty whole suites while the run still exits 0.
    """
    if os.environ.get("OCX_TESTS_NO_REGISTRY") == "1":
        pytest.skip("OCX_TESTS_NO_REGISTRY=1: registry:2 fixture not started")
    skip_without_docker(f"{fixture} (the compose 'mirror-registry')")
    if registry_is_reachable(address):
        return address
    if shutil.which("docker") is None:
        pytest.fail(
            f"{fixture}: no registry:2 at {address} and no `docker` to start the compose "
            "'mirror-registry'; a job without Docker declares OCX_TEST_NO_DOCKER=1",
            pytrace=False,
        )
    compose_up_service("mirror-registry")
    if not _wait_for_reachable(lambda: registry_is_reachable(address)):
        raise RuntimeError(
            f"mirror-registry at {address} did not become reachable after "
            f"`docker compose up -d mirror-registry` and a bounded retry"
        )
    return address


@pytest.fixture(scope="session")
def registry() -> str:
    return _zot_address("registry")


@pytest.fixture(scope="session")
def mirror_registry() -> str:
    """Session-scoped mirror registry (localhost:5001, the registry:2 service).

    Tests in ``test_oci_registry_mirror.py`` declare this as a fixture
    dependency; it is the only thing that starts the compose service.
    """
    return _mirror_address(
        os.environ.get("MIRROR_REGISTRY", _DEFAULT_MIRROR_REGISTRY), "mirror_registry"
    )


@pytest.fixture(scope="session")
def target_registry() -> str:
    """Session-scoped promotion target (the session's second zot).

    A cross-registry ``ocx package copy`` needs a target that implements the OCI
    1.1 Referrers API, so it cannot be ``mirror_registry`` -- that one is
    registry:2 and deliberately has none. Consumed by
    ``tests/test_package_copy.py``.
    """
    return _zot_address("target")


@pytest.fixture(scope="session")
def legacy_registry() -> str:
    """Session-scoped referrers-NEGATIVE fixture (registry:2, localhost:5001).

    A real OCI Distribution v2 registry that does NOT implement the OCI 1.1
    Referrers API: ``GET /v2/<name>/referrers/<digest>`` returns 404. Consumed
    by ``test_referrers_capability.py`` to assert the clean fallback-index path
    against a genuine v2 registry, and by ``test_referrers_smoke.py``. Backed by
    the SAME compose ``mirror-registry`` service as ``mirror_registry``.
    """
    return _mirror_address(
        os.environ.get("LEGACY_REGISTRY", _DEFAULT_MIRROR_REGISTRY), "legacy_registry"
    )


@pytest.fixture(scope="session")
def ocx_binary() -> Path:
    """The `ocx` under test.

    Registry-independent opt-out (``OCX_TESTS_NO_REGISTRY=1``): the runner that
    sets it -- ``build-windows-shims.yml``'s ``acceptance-windows`` job -- builds
    only ``ocx_shim`` and starts no registry, so an ``ocx`` is neither present
    nor useful there (every test reaching this fixture also depends on
    ``registry``, which skips under the same flag). Both halves are *observed*
    before the skip: the flag is set and the binary is genuinely absent. On any
    runner that does build one, a missing binary stays a hard failure -- a
    silent skip of the whole acceptance suite is the false green this guard
    must not create.
    """
    if env_path := os.environ.get("OCX_COMMAND"):
        p = Path(env_path)
    else:
        p = PROJECT_ROOT / "test" / "bin" / "ocx"
        if sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")
    if not p.exists() and os.environ.get("OCX_TESTS_NO_REGISTRY") == "1":
        pytest.skip(
            f"OCX_TESTS_NO_REGISTRY=1 and no ocx binary at {p}: this runner "
            "builds ocx_shim only (see build-windows-shims.yml)"
        )
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


@pytest.fixture()
def forward_proxy(registry: str) -> Iterator[forward_proxy_mod.ForwardProxy]:
    """A per-test absolute-form HTTP forward proxy (`src/forward_proxy.py`)
    that aliases `forward_proxy_mod.PHANTOM_REGISTRY` onto the real `registry`
    fixture's loopback port.

    Function-scoped and ephemeral-port, like `fake_forge` above — no xdist
    group, no port knob needed. `OcxRunner` builds its per-invocation
    environment from scratch (`OcxRunner.__init__` / `run(env_overrides=...)`),
    so an ambient developer `HTTP_PROXY`/`HTTPS_PROXY` can never leak into a
    test; a test that needs proxying sets it explicitly via `env_overrides`.
    """
    host, port = registry.split(":", 1)
    with forward_proxy_mod.running(
        {forward_proxy_mod.PHANTOM_REGISTRY: (host, int(port))}
    ) as proxy:
        yield proxy


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
