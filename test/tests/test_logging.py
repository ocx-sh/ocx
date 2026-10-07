"""``OCX_LOG`` still names every library crate as a log target.

Library code calls the ``log`` facade; the CLI's tracing subscriber renders
those records on stderr through ``tracing_log::LogTracer``. A crate move can
break either half silently — a record that no longer reaches the subscriber, or
a directive that no longer names a target. The subscriber prints the level but
no target (``with_target(false)``), so each row asserts on levels alone: a line
at the row's level renders under ``OCX_LOG=<target>=<level>``, and none under
the next stricter level. Message text stays free to change.
"""

from __future__ import annotations

import re
import subprocess

import pytest

LEVELS = ("trace", "debug", "info", "warn", "error")
# With debug or trace enabled, the compact format (no ANSI on a pipe):
# `<timestamp> <LEVEL> <spans and message>`.
TRACING_LINE = re.compile(r"^\S+\s+(TRACE|DEBUG|INFO|WARN|ERROR)\s+(.*)$")
# Otherwise cargo-style: `error: msg`, `warning: msg`, an info line bare.
HUMAN_PREFIX = {"error": "error: ", "warn": "warning: "}


def _level_of(line: str) -> tuple[str, str]:
    """``(level, message)`` of one stderr line, in either rendering."""
    if m := TRACING_LINE.match(line):
        return m[1].lower(), m[2]
    for level, prefix in HUMAN_PREFIX.items():
        if line.startswith(prefix):
            return level, line.removeprefix(prefix)
    return "info", line


def _lines_at(stderr: str, level: str) -> list[str]:
    """The span-and-message part of every tracing line rendered at ``level``."""
    parsed = (_level_of(line) for line in stderr.splitlines())
    return [message for line_level, message in parsed if line_level == level]


#: Replaced with ``published_package.fq`` before the row runs. Matched by
#: identity, not equality, so it can never rewrite a coincidentally equal argument.
PACKAGE = "<published package>"
#: Replaced with ``fake_forge.base_url`` in ``extra_env``, matched by identity.
#: ``ocx_announce``'s lines sit behind a forge round-trip; without the fake forge
#: the row would reach ``api.github.com`` and take its green from the internet.
FORGE = "<fake forge base url>"
#: Replaced with a ``tmp_path`` child: ``--out`` needs a directory that exists.
OUT_DIR = "<claim out dir>"
#: Header cosign writes and `PemKeyBackend::from_encrypted_pem` dispatches on;
#: the body is deliberately not a key.
BAD_ENCRYPTED_KEY_PEM = (
    "-----BEGIN ENCRYPTED SIGSTORE PRIVATE KEY-----\n"
    "bm90IGEga2V5\n"
    "-----END ENCRYPTED SIGSTORE PRIVATE KEY-----\n"
)

#: Rows whose line renders *before* a wire call or refusal that then fails the
#: verb. The value is the exact status when the refusal has one path to reach
#: it, ``None`` where any failure is tolerated. ``ocx_announce`` exits 78: the
#: SSRF pre-flight refuses the loopback ``--repository`` before any socket
#: opens; a row that only asked for non-zero would stay green if the refusal
#: moved to one that opened the socket first.
EXPECT_EXIT: dict[str, int | None] = {
    "ocx_oci": None,
    "ocx_trust": None,
    "ocx_sign": None,
    "ocx_announce": 78,
}
#: The fake forge starts with no users, so a claim run exits 79 (`OwnerUnknown`)
#: before it logs anything unless the owner is seeded. The login must not exist
#: on real GitHub: a run that lost the forge override then 404s instead of going
#: green against `api.github.com`.
SEEDED_OWNERS = {"ocx_announce": ("ocx-sh-nonexistent-claim-fixture", 4242424242)}

CATALOG = ("--offline", "index", "catalog")
# One row per library crate hosting `log::` call sites: (directive target, level
# of a line the argv renders, extra environment, argv). Each argv is the cheapest
# one reaching that crate's line with no network. A stale target renders nothing
# and FAILS — it never skips. `test/lint/test_logging_structure.py` holds the
# table complete against the crates that log.
LIBRARY_TARGETS = [
    # `config setup --managed-config ""` warns while OCX_MANAGED_CONFIG is exported.
    (
        "ocx_setup",
        "warn",
        {"OCX_MANAGED_CONFIG": "ghcr.io/ocx-sh/nothing:0"},
        ("config", "setup", "--managed-config", ""),
    ),
    # Every invocation reads OCX_NO_CONFIG through `env::flag`, which warns on a non-boolean.
    ("ocx_util", "warn", {"OCX_NO_CONFIG": "maybe"}, CATALOG),
    # `shell revoke` on an unstamped project reports through `UserInterface::status`.
    ("ocx_console", "info", {}, ("--offline", "shell", "revoke")),
    # The credential lookup runs before the connect to the closed port 127.0.0.1:1.
    ("ocx_oci", "debug", {}, ("package", "install", "127.0.0.1:1/x:1")),
    # A `--key` is compiled before anything is fetched; a bad PEM logs its rejection.
    (
        "ocx_trust",
        "debug",
        {"__OCX_TESTING_BAD_KEY_PEM": "-----BEGIN PUBLIC KEY-----\nnot a key\n-----END PUBLIC KEY-----"},
        ("package", "verify", "--key", "env://__OCX_TESTING_BAD_KEY_PEM", "127.0.0.1:1/x:1"),
    ),
    # OCX_PROJECT="" takes the loader's escape hatch, logged before any file is read.
    ("ocx_config", "debug", {"OCX_PROJECT": ""}, CATALOG),
    ("ocx_index", "debug", {}, CATALOG),
    # EnvFilter matches targets by prefix, so `ocx_package` also selects
    # `ocx_package_manager`; cascade repair logs nothing from the manager at debug.
    ("ocx_package", "debug", {}, ("package", "cascade", "repair", PACKAGE)),
    (
        "ocx_sign",
        "debug",
        {"__OCX_TESTING_BAD_ENCRYPTED_KEY_PEM": BAD_ENCRYPTED_KEY_PEM},
        ("package", "sign", "--key", "env://__OCX_TESTING_BAD_ENCRYPTED_KEY_PEM", "127.0.0.1:1/x:1"),
    ),
    # No state-free `debug!` exists in this crate; `about` runs the shell-detection walk.
    ("ocx_shell", "trace", {}, ("about",)),
    # `shell state` reads the consent stamp of a project that has none.
    ("ocx_project", "debug", {}, ("shell", "state")),
    (
        "ocx_announce",
        "info",
        # Pinned so the canonical `ocx.sh` name does not follow the harness default.
        {"__OCX_TESTING_FORGE_BASE_URL": FORGE, "OCX_DEFAULT_REGISTRY": "ocx.sh"},
        (
            "package",
            "claim",
            "--repository",
            "oci://127.0.0.1/acme/widget",
            "--owner",
            "ocx-sh-nonexistent-claim-fixture:4242424242",
            "--out",
            OUT_DIR,
            "acme/widget",
        ),
    ),
    # The implicit `$OCX_HOME/ocx.lock` root is always walked and always absent on a fresh home.
    ("ocx_package_manager", "debug", {}, ("--offline", "clean")),
    # Every store line sits behind on-disk state; install links the candidate symlink.
    ("ocx_store", "debug", {}, ("package", "install", PACKAGE)),
]


def _run(ocx, argv: tuple[str, ...], check: bool = True, **env: str) -> subprocess.CompletedProcess[str]:
    return ocx.plain(*argv, env_overrides=env, check=check)


def test_debug_level_reaches_library_events(ocx):
    """``--log-level debug`` renders every line ``OCX_LOG=ocx_index=debug`` selects, and ``info`` renders none."""
    library = _lines_at(_run(ocx, CATALOG, OCX_LOG="ocx_index=debug").stderr, "debug")
    assert library, "OCX_LOG=ocx_index=debug rendered no DEBUG line, so this test compares nothing"
    flag = _lines_at(ocx.plain(*CATALOG, log_level="debug").stderr, "debug")
    # `endswith`: under the flag, CLI spans the library-only filter drops prefix the same message.
    missing = [line for line in library if not any(other.endswith(line) for other in flag)]
    assert not missing, f"--log-level debug did not render these library lines: {missing!r}; rendered={flag!r}"
    quiet = _lines_at(ocx.plain(*CATALOG, log_level="info").stderr, "debug")
    assert not quiet, f"--log-level info rendered DEBUG lines: {quiet!r}"


@pytest.mark.parametrize(
    ("target", "level", "extra_env", "argv"), LIBRARY_TARGETS, ids=[t for t, _, _, _ in LIBRARY_TARGETS]
)
def test_env_filter_selects_library_target(request, ocx, tmp_path, target, level, extra_env, argv):
    """``OCX_LOG=<target>=<level>`` renders a line at that level; the next stricter level renders none."""
    crate = target
    check = crate not in EXPECT_EXIT
    # Fixtures requested per row: a published package and a fake forge each cost a round-trip or a port.
    if any(item is PACKAGE for item in argv):
        argv = tuple(request.getfixturevalue("published_package").fq if item is PACKAGE else item for item in argv)
    if any(item is OUT_DIR for item in argv):
        out = tmp_path / "claim-out"
        out.mkdir()
        argv = tuple(str(out) if item is OUT_DIR else item for item in argv)
    if any(value is FORGE for value in extra_env.values()):
        forge = request.getfixturevalue("fake_forge")
        if crate in SEEDED_OWNERS:
            forge.seed_user(*SEEDED_OWNERS[crate])
        extra_env = {key: (forge.base_url if value is FORGE else value) for key, value in extra_env.items()}
    # The runner keeps the real HOME, so credential lookup would read the host's
    # docker config; a `credsStore` there changes which `ocx_oci` arm runs.
    docker_config = tmp_path / "docker"
    docker_config.mkdir()
    extra_env = {**extra_env, "DOCKER_CONFIG": str(docker_config)}

    completed = _run(ocx, argv, check=check, OCX_LOG=f"{target}={level}", **extra_env)
    expected_status = EXPECT_EXIT.get(crate)
    if expected_status is not None:
        assert completed.returncode == expected_status, (
            f"{target} row exited {completed.returncode}, not the {expected_status} its refusal "
            f"classifies to; stderr={completed.stderr!r}"
        )
    assert _lines_at(completed.stderr, level), (
        f"OCX_LOG={target}={level} rendered no {level.upper()} line — is {target!r} still a log target?; "
        f"stderr={completed.stderr!r}"
    )

    if crate == "ocx_package":
        # The prefix also selects ocx_package_manager: its own run must stay silent, or the line above proves nothing.
        manager_home = tmp_path / "manager-home"
        manager_home.mkdir()
        manager = _run(
            ocx, argv, check=check, OCX_LOG=f"ocx_package_manager={level}", OCX_HOME=str(manager_home), **extra_env
        )
        assert not _lines_at(manager.stderr, level), f"ocx_package_manager logs here too; stderr={manager.stderr!r}"

    # A fresh OCX_HOME: idempotent work (the store's symlink) renders nothing the
    # second time, which would make this absence hold whatever the filter says.
    control_home = tmp_path / "control-home"
    control_home.mkdir()
    stricter = LEVELS[LEVELS.index(level) + 1]
    control_run = _run(
        ocx, argv, check=check, OCX_LOG=f"{target}={stricter}", OCX_HOME=str(control_home), **extra_env
    )
    # A control that failed earlier than the selected run never reached the line's code path.
    assert control_run.returncode == completed.returncode, (
        f"control exited {control_run.returncode}, the selected run {completed.returncode}; "
        f"stderr={control_run.stderr!r}"
    )
    control = control_run.stderr
    assert not _lines_at(control, level), (
        f"OCX_LOG={target}={stricter} still rendered {level.upper()} lines; stderr={control!r}"
    )


def test_env_filter_naming_no_crate_switches_every_crate_off(ocx):
    """``OCX_LOG=<unknown target>=<level>`` renders nothing from any real crate, at any level."""
    library = _lines_at(_run(ocx, CATALOG, OCX_LOG="ocx_index=debug").stderr, "debug")
    assert library, "OCX_LOG=ocx_index=debug rendered no DEBUG line, so this test compares nothing"
    for level in ("trace", "debug", "info"):
        stderr = _run(ocx, CATALOG, OCX_LOG=f"ocx_nonexistent_target={level}").stderr
        assert not _lines_at(stderr, level), f"an unknown target at {level} still rendered lines; stderr={stderr!r}"
