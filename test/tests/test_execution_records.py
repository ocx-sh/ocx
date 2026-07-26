# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the exec-time resolution record (issue #214).

Specification mode (contract-first TDD)
---------------------------------------
Every test here is written from the design record, not from the
implementation: ``.claude/state/plans/plan_exec_resolution_record.md`` §3.2
(the acceptance table) and ``.claude/artifacts/adr_exec_resolution_record.md``
(the record format, the three exemplary records, the ``[records]`` block, the
filename grammar). They are expected to FAIL until WP-7b lands.

What is deliberately NOT here (plan §3.2 "Moved to unit level")
---------------------------------------------------------------
The `system_locked` clamp itself (a locked block ignoring env and CLI, per
field): pytest cannot write ``/etc/ocx/config.toml``, and the
``__OCX_TESTING_SYSTEM_CONFIG`` seam only redirects the *path* the loader
treats as the SYSTEM tier — enough for test 30, which is about that tier being
consulted at all, not about the clamp's field table. No-clobber publication:
the harness cannot pre-create the target, because the default filename carries
milliseconds and a not-yet-spawned pid. Both source-structure firewall tests:
they have no runtime observable.

Configuration contracts encoded here
------------------------------------
* ``required`` is **config-file-only** — there is no ``--records-required``
  flag and no ``OCX_RECORDS_REQUIRED`` env var. It is set in a config file
  (here ``$OCX_HOME/config.toml``, the tier pytest can actually write).
* Flags are ``--records-dir DIR`` / ``--records-name TEMPLATE`` and exist on
  ``ocx exec`` and ``ocx package exec`` only. ``ocx launcher exec`` takes none —
  it is reached through ``OCX_RECORDS_DIR``.
* Env vars are ``OCX_RECORDS_DIR`` / ``OCX_RECORDS_NAME``.
* One record = one compact single-line JSON document = one file (format rule
  8). ``digest`` values are bare lowercase hex, no ``sha256:`` prefix (rule 1).
* A collector ignores ``.tmp*`` in the sink; so does ``_record_paths``.

Test-only seam this file assumes
--------------------------------
``__OCX_TESTING_RECORDS_FAIL_PROBES`` — comma-separated record key paths whose
best-effort probe is forced to fail. Vocabulary is the record's own key paths:
``host.name``, ``os.type``, ``process.arch``, ``process.user.id``,
``process.user.name``, ``process.parent.pid``, ``process.working_directory``.
One variable rather than one per probe, so the seam's vocabulary is the wire
format's own.

Test inventory
--------------
 1. test_exec_frame_emits_record                                 — three frames
 2. test_package_exec_frame_emits_record                          — three frames
 3. test_launcher_exec_frame_emits_record                          — three frames
 4. test_entrypoint_invocation_emits_two_records_joined_on_digest  — entrypoint
 5. test_non_entrypoint_invocation_emits_one_binary_record         — non-entrypoint
 6. test_direct_launcher_invocation_records_degraded_identity      — degraded
 7. test_auto_installed_package_is_named_in_resolution             — auto-install
 8. test_already_installed_package_has_no_auto_install_markers     — auto-install
 9. test_unwritable_sink_warns_and_runs_when_not_required          — warn open
10. test_concurrent_invocations_each_write_a_complete_record       — concurrency
11. test_required_true_with_unwritable_sink_exits_74               — fail closed
12. test_records_dir_precedence_config_then_env_then_flag          — precedence
13. test_unknown_name_placeholder_exits_78                         — placeholder
14. test_record_pid_is_the_process_that_runs_the_tool              — PID
15. test_record_written_when_every_best_effort_probe_fails         — best-effort
16. test_package_test_of_an_entrypoint_package_writes_no_record    — exclusion
17. test_sink_designated_through_a_symlink_records_at_its_target   — sink pinning
18. test_emitted_record_validates_against_the_published_schema     — schema bind
19. test_record_file_is_readable_only_by_its_owner                 — file mode
20. test_records_dir_flag_alone_reaches_the_launcher_re_entry      — forwarding
21. test_child_exit_code_is_propagated                             — exit code
22. test_signalled_child_reports_the_signal                        — exit code
23. test_recording_run_still_propagates_the_child_exit_code        — exit code
24. test_multi_package_exec_names_only_the_absent_package          — attribution
25. test_root_digest_is_the_platform_leaf_never_the_index          — rule 5
26. test_cached_multi_platform_root_records_the_platform_it_selected — rule 5
27. test_dependency_entries_carry_no_platform_and_no_arch_qualifier — rule 5
28. test_registries_name_the_content_host_not_the_logical_namespace — indirection
29. test_required_true_with_no_sink_is_a_configuration_error            — fail closed
30. test_a_symlinked_system_config_refuses_the_launch                   — fail closed
31. test_a_forged_scratch_pkg_root_cannot_escape_a_required_policy      — fail closed
32. test_a_plain_http_registry_is_named_in_insecure_registries      — transport
33. test_a_patched_invocation_records_its_companion_and_snapshot    — patch tier
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import jsonschema
import pytest

from src import static_index
from src.helpers import (
    make_package,
    make_package_with_entrypoints,
    resolved_metadata_path,
    write_ocx_toml,
)
from src.registry import fetch_manifest_digest, fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo, current_platform
from src.shell_eval import run_after_sourcing

# ---------------------------------------------------------------------------
# Exit codes — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_IO = 74      # unwritable sink under `required = true`
EXIT_CONFIG = 78  # unknown filename placeholder — a config error, not an I/O one

BARE_SHA256 = re.compile(r"^[0-9a-f]{64}$")

RECORD_KIND = "sh.ocx.execution-record"

# The best-effort probe seam (see module docstring).
FAIL_PROBES = "__OCX_TESTING_RECORDS_FAIL_PROBES"

# Every key path the seam understands — the record's own vocabulary. Listed in
# full so "all best-effort probes failed" is literally that, not a sample.
ALL_PROBES = (
    "host.name",
    "os.type",
    "process.arch",
    "process.user.id",
    "process.user.name",
    "process.parent.pid",
    "process.working_directory",
)


# ---------------------------------------------------------------------------
# Sink helpers
# ---------------------------------------------------------------------------


def _sink(tmp_path: Path, name: str) -> Path:
    """Create and return an empty records sink directory.

    Resolved so the path a test hands ocx is already the one ocx pins: the sink
    is canonicalized once at policy resolution, and macOS reaches ``$TMPDIR``
    through ``/var`` -> ``/private/var``. Handing over the unresolved spelling
    would make every path a test later asserts on differ from the pinned one for
    a reason no test here is about. The Rust unit tests canonicalize for the
    same reason.
    """
    path = tmp_path / f"records-{name}"
    path.mkdir(parents=True, exist_ok=True)
    return path.resolve()


def _unwritable_sink(tmp_path: Path, name: str) -> Path:
    """Return a sink path that can never accept a record.

    A regular file standing where the sink directory should be. Preferred over
    ``chmod 0o500``: the acceptance suite may run as root in a container, where
    permission bits are advisory and the sink would be writable after all.

    Resolved for the same reason as ``_sink``.
    """
    path = tmp_path / f"not-a-dir-{name}"
    path.write_text("this is a file, not a directory\n")
    return path.resolve()


def _record_paths(sink: Path) -> list[Path]:
    """Every published record in ``sink``, ``.tmp*`` staging files excluded."""
    if not sink.is_dir():
        return []
    return sorted(
        path
        for path in sink.iterdir()
        if path.is_file() and not path.name.startswith(".tmp")
    )


def _read_records(sink: Path) -> list[dict]:
    """Parse every record in ``sink``, oldest first.

    Also enforces format rule 8 on each file: exactly one compact JSON
    document, on a single line.
    """
    records: list[dict] = []
    for path in _record_paths(sink):
        body = path.read_text()
        stripped = body.strip()
        assert stripped, f"record file is empty: {path}"
        assert "\n" not in stripped, (
            "a record is one compact single-line JSON document (format rule 8); "
            f"{path.name} spans multiple lines:\n{body[:400]}"
        )
        records.append(json.loads(stripped))
    records.sort(key=lambda record: record.get("recordedAt", ""))
    return records


def _one_record(sink: Path) -> dict:
    """Assert exactly one record landed in ``sink`` and return it."""
    records = _read_records(sink)
    assert len(records) == 1, (
        f"expected exactly one record in {sink}; got {len(records)}: "
        f"{[path.name for path in _record_paths(sink)]}"
    )
    return records[0]


def _root_entry(record: dict) -> dict:
    """The ``packages[]`` entry annotated ``sh.ocx.role: root``."""
    roots = [
        entry
        for entry in record["packages"]
        if entry.get("annotations", {}).get("sh.ocx.role") == "root"
    ]
    assert roots, f"record carries no root package entry: {record['packages']}"
    return roots[0]


def _root_digest(record: dict) -> str:
    """The root package's sha256, asserted bare lowercase hex (format rule 1)."""
    digest = _root_entry(record)["digest"]["sha256"]
    assert BARE_SHA256.match(digest), (
        "digest values are bare lowercase hex with no transport prefix "
        f"(format rule 1); got {digest!r}"
    )
    return digest


def _entries_with_role(record: dict, role: str) -> list[dict]:
    """Every ``packages[]`` entry annotated with ``sh.ocx.role: <role>``."""
    return [
        entry
        for entry in record["packages"]
        if entry.get("annotations", {}).get("sh.ocx.role") == role
    ]


def _purl_qualifiers(uri: str) -> dict[str, list[str]]:
    """The query qualifiers of a purl, e.g. ``repository_url`` / ``arch``."""
    return parse_qs(urlsplit(uri).query)


# ---------------------------------------------------------------------------
# The published JSON schema
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parents[2]
SCHEMA_BINARY = PROJECT_ROOT / "target" / "release" / "ocx_schema"


@pytest.fixture(scope="module")
def execution_record_schema() -> dict:
    """The published execution-record JSON schema, as the generator emits it.

    Read from the ``ocx_schema`` binary rather than from
    ``website/src/public/schemas/`` so the schema under test is the one this
    tree generates, not whatever copy happens to be checked in. The binary is
    the same artefact ``test_schema_generation.py`` consumes; when it is absent
    that module already skips, and so does this one test.
    """
    if not SCHEMA_BINARY.exists():
        pytest.skip(
            f"{SCHEMA_BINARY} is absent; run `task schema` (or "
            "`cargo build --release -p ocx_schema`) to generate it"
        )
    result = subprocess.run(
        [str(SCHEMA_BINARY), "execution-record"],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert result.returncode == 0, (
        f"ocx_schema execution-record failed (exit {result.returncode})\n"
        f"stderr: {result.stderr}"
    )
    return json.loads(result.stdout)


# ---------------------------------------------------------------------------
# Invocation helpers
# ---------------------------------------------------------------------------


def _run_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ocx with an explicit CWD (drives the ``ocx.toml`` walk)."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        timeout=120,
        check=False,
    )


def _write_home_config(ocx: OcxRunner, body: str) -> Path:
    """Write ``$OCX_HOME/config.toml`` — the config tier pytest can write."""
    path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    path.write_text(body)
    return path


def _toml_path(path: Path) -> str:
    """Render a path as a TOML basic string (escapes Windows backslashes)."""
    return json.dumps(str(path))


def _package_root(ocx: OcxRunner, short: str) -> Path:
    """The on-disk package root of an installed package.

    ``ocx package which`` reports ``{"<id>": {"path": ..., "kind": ...}}`` —
    the value grew from a bare path string into an object when lazy shims
    became locatable, so ``kind`` distinguishes a materialized package root
    from a generated shim tree. This helper wants the former.
    """
    result = ocx.json("package", "which", short)
    located = result.get(short) if isinstance(result, dict) else None
    assert isinstance(located, dict), (
        f"ocx package which must report a located-path object for {short!r}; got {located!r}"
    )
    assert located.get("kind") == "package", (
        f"the record frames under test resolve materialized packages, not {located.get('kind')!r} entries"
    )
    return Path(located["path"])


def _project_with_tool(ocx: OcxRunner, tmp_path: Path, pkg: PackageInfo) -> Path:
    """Create a locked single-binding project directory for ``ocx exec``."""
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\n{pkg.repo} = "{ocx.registry}/{pkg.repo}:{pkg.tag}"\n'
    )
    lock = _run_in(ocx, project, "lock")
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    )
    return project


# ---------------------------------------------------------------------------
# 1-3. Three recording frames — exec, package exec, launcher exec
# ---------------------------------------------------------------------------


def test_exec_frame_emits_record(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``ocx exec --records-dir`` writes one record for the project tier.

    Also pins the envelope every record carries: ``schemaVersion`` is a string
    (rule 7), ``kind`` is the frozen record type, and ``frame.command`` names
    the frame.
    """
    project = _project_with_tool(ocx, tmp_path, published_package)
    sink = _sink(tmp_path, "exec")

    result = _run_in(ocx, project, "exec", "--records-dir", str(sink), "--", "hello")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert isinstance(record["schemaVersion"], str), (
        f"schemaVersion is a string (format rule 7); got {record['schemaVersion']!r}"
    )
    assert record["kind"] == RECORD_KIND, (
        f"every record carries kind={RECORD_KIND!r}; got {record['kind']!r}"
    )
    assert record["frame"]["command"] == "exec", (
        f"the exec frame reports frame.command='exec'; got {record['frame']}"
    )
    assert record["scope"]["tier"] == "project", (
        f"ocx exec records the project tier; got scope={record['scope']}"
    )
    assert record["packages"], "a record always carries the resolved package closure"

    # `declarationDigest`, not `digest`: the value hashes the `ocx.toml`
    # declarations the lock was generated from, never the lock's contents. Two
    # runs whose declarations agree share it even when they resolved different
    # closures, so a consumer that read it as a closure identity would be wrong
    # — the name is the thing that stops them.
    lock = record["scope"]["lock"]
    assert set(lock) == {"path", "declarationDigest"}, (
        f"the lock reference carries exactly path + declarationDigest; got {lock}"
    )
    assert lock["path"] == str(project / "ocx.lock"), (
        f"the lock reference names the project's own lock; got {lock['path']!r}"
    )
    assert BARE_SHA256.match(lock["declarationDigest"]["sha256"]), (
        "the declaration digest is bare lowercase hex (format rule 1); got "
        f"{lock['declarationDigest']}"
    )


def test_package_exec_frame_emits_record(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``ocx package exec --records-dir`` writes one record for the OCI tier."""
    sink = _sink(tmp_path, "pkgexec")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx package exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert record["kind"] == RECORD_KIND
    assert record["frame"]["command"] == "package exec", (
        f"the package exec frame reports frame.command='package exec'; got {record['frame']}"
    )
    assert record["scope"]["tier"] == "package", (
        f"ocx package exec records the package tier; got scope={record['scope']}"
    )

    # The argv is deliberately absent from v1 (ADR amendment 1): a command line
    # carries access tokens and passwords often enough that a record collected
    # fleet-wide into a central store must not contain one. Asserted here rather
    # than left to review, because nothing else would notice it coming back.
    process = record["process"]
    assert "args" not in process, (
        f"process.args is not part of v1 — argv is a secret-bearing surface; got {process}"
    )
    host_os, host_arch = current_platform().split("/")
    assert process["arch"] == host_arch, (
        "the architecture recorded is the *process's*, under `process`, in the "
        f"OCI vocabulary; expected {host_arch!r}, got {process.get('arch')!r}"
    )
    assert "arch" not in record["host"], (
        "the host block carries the machine name only; the architecture moved to "
        f"`process.arch`, which is the one that is true of what ran; got {record['host']}"
    )

    # A frame that composed an environment reports the context it composed it
    # from; the launcher frame omits all four (asserted in the launcher tests).
    resolution = record["resolution"]
    assert record["os"]["type"] == host_os, (
        f"os.type is the OS family; expected {host_os!r}, got {record['os']}"
    )
    # `requestedPlatform`, not `platform`: the field is the platform resolution
    # was ASKED for, and each package's `sh.ocx.platform` is what it resolved to.
    # A leading `os/arch` match is the whole assertion — the canonical grammar
    # appends host ABI features (`+libc.glibc`) that vary by host.
    assert resolution["requestedPlatform"].startswith(current_platform()), (
        "resolution.requestedPlatform is the platform resolution was asked for, "
        f"in the canonical grammar; got {resolution['requestedPlatform']!r}"
    )
    assert resolution["registries"] == [ocx.registry], (
        "resolution.registries names the content registries the roots came from; "
        f"got {resolution.get('registries')!r}"
    )
    assert resolution["mirrors"] == {}, (
        "a composing frame with no mirrors reports an empty object — 'composed "
        f"with none', not 'no mirror context'; got {resolution.get('mirrors')!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_launcher_exec_frame_emits_record(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx launcher exec`` records too, reached only via ``OCX_RECORDS_DIR``.

    The launcher frame carries no ``--records-dir`` flag by design, so the
    environment variable is the whole configuration surface here.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    pkg_root = _package_root(ocx, pkg.short)
    sink = _sink(tmp_path, "launcher")

    result = ocx.run(
        "launcher", "exec", str(pkg_root), "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(sink)},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"launcher exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert record["kind"] == RECORD_KIND
    assert record["frame"]["command"] == "launcher exec", (
        f"the launcher frame reports frame.command='launcher exec'; got {record['frame']}"
    )
    assert record["scope"]["tier"] == "launcher", (
        f"launcher exec records the launcher tier; got scope={record['scope']}"
    )

    # A launcher re-entry composed nothing, so it has no registry, mirror or
    # managed-config context of its own — a different statement from "composed
    # with none", which the package tier records as an empty collection. The
    # requested platform is emitted as an explicit null rather than omitted, and
    # never fabricated from the host: fabricating it would make the record lie in
    # exactly the audit that matters.
    resolution = record["resolution"]
    assert resolution["requestedPlatform"] is None, (
        "a launcher frame has no platform context and must say null, never guess; "
        f"got {resolution['requestedPlatform']!r}"
    )
    for absent in ("registries", "mirrors", "managedConfig"):
        assert absent not in resolution, (
            f"a launcher frame omits {absent} entirely rather than emitting an "
            f"empty one; got resolution={resolution}"
        )


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase")
def test_launcher_shim_frame_emits_a_record_and_names_the_pull_only_once(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The fourth launching frame records, and ``autoInstalled`` discriminates.

    ``ocx launcher shim`` runs a deferred tool's **first** invocation — the
    moment content is downloaded and the binary actually executes. Under
    ``lazy-mode = "always"`` a fleet would otherwise get no record for exactly
    the invocation an auditor cares most about.

    Two invocations, not one, because a single sample cannot tell a derived
    ``autoInstalled`` from a hardcoded one: the first pulls and must name the
    tool, the second finds the same package in the store and must name nothing.
    Both halves are the assertion.
    """
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        bins=["hello"],
        binaries=["hello"],
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            }
        ],
    )
    project = tmp_path / "lazy-proj"
    project.mkdir()
    write_ocx_toml(project, f'lazy-mode = "always"\n\n[tools]\nhello = "{pkg.fq}"\n')
    # `--no-pull` is what leaves the store genuinely cold, so the first trigger
    # below is a real materialization rather than a store hit.
    lock = _run_in(ocx, project, "lock", "--no-pull")
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx lock --no-pull failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    )

    export = _run_in(ocx, project, "env", "--shell=sh")
    assert export.returncode == EXIT_SUCCESS, f"ocx env --shell=sh failed:\n{export.stderr}"

    # A generated launcher re-enters `${OCX_BINARY_PIN:-ocx}`, so the pin has to
    # name the binary under test — the ambient PATH would find a different build.
    def trigger(sink: Path) -> None:
        env = dict(ocx.env)
        env["OCX_BINARY_PIN"] = str(ocx.binary)
        env["OCX_RECORDS_DIR"] = str(sink)
        result = run_after_sourcing(export.stdout, "hello", cwd=project, env=env)
        assert result.returncode == EXIT_SUCCESS, (
            f"the shim trigger failed; rc={result.returncode}\nstderr:\n{result.stderr}"
        )

    cold = _sink(tmp_path, "shim-cold")
    trigger(cold)
    record = _one_record(cold)
    assert record["kind"] == RECORD_KIND
    assert record["frame"]["command"] == "launcher shim", (
        f"the shim frame reports frame.command='launcher shim'; got {record['frame']}"
    )
    assert record["frame"]["identity"] == "complete", (
        "a shim is baked with the tool's pinned identifier, so unlike `launcher exec` "
        f"it resolves logical identity; got {record['frame']}"
    )
    assert record["scope"]["tier"] == "package", (
        f"a shim composes the package tier; got scope={record['scope']}"
    )
    assert record["packages"], "a shim record carries the resolved closure it composed"

    # The DIGEST-pinned spelling, not the `repo:tag` one: a shim is baked with
    # the pinned identifier the lock resolved, and the pull it triggers is
    # addressed by that digest with no tag resolve at all. Asserted against the
    # record's own root digest so the two halves of one record must agree.
    assert record["resolution"]["autoInstalled"] == [
        f"{ocx.registry}/{pkg.repo}@sha256:{_root_digest(record)}"
    ], (
        "the first invocation downloaded the content, which is the one state no "
        f"pull-time record can capture; got resolution={record['resolution']}"
    )

    # The warm half goes through the wire ABI directly rather than through
    # `PATH` again: materialization does not retire the shim tree, but it does
    # put the package's real `bin/` on the composed `PATH` above it, so a second
    # `hello` runs the tool with no ocx frame at all and records nothing. The
    # shim is still reachable, and reaching it is what this half tests.
    pinned = record["resolution"]["autoInstalled"][0]
    warm = _sink(tmp_path, "shim-warm")
    result = ocx.run(
        "launcher", "shim", pinned, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(warm)},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"launcher shim must succeed on a materialized tool; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    second = _one_record(warm)
    assert second["frame"]["command"] == "launcher shim"
    assert "autoInstalled" not in second["resolution"], (
        "the second invocation found the package already in the store, so it "
        f"materialized nothing; got resolution={second['resolution']}"
    )


# ---------------------------------------------------------------------------
# 4-5. Entrypoint splits into two records; a plain binary into one
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher re-entry test")
def test_entrypoint_invocation_emits_two_records_joined_on_digest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Executing an entrypoint records twice: the launcher, then the leaf binary.

    ADR option L1. The outer frame resolves the generated launcher under
    ``entrypoints/`` and owns the logical identity; the inner ``launcher exec``
    re-entry resolves the real binary under ``content/``. They are complementary
    halves and join on the package's content digest, which both carry.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    sink = _sink(tmp_path, "entrypoint")

    result = ocx.run(
        "package", "exec", pkg.short, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(sink)},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"entrypoint invocation must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    records = _read_records(sink)
    assert len(records) == 2, (
        "an entrypoint invocation records at BOTH frames — the launcher and the "
        f"leaf binary; got {len(records)} record(s): "
        f"{[record.get('executable') for record in records]}"
    )

    kinds = [record["executable"]["sh.ocx.kind"] for record in records]
    assert sorted(kinds) == ["binary", "launcher"], (
        f"expected one launcher record and one binary record; got {kinds}"
    )
    outer = next(r for r in records if r["executable"]["sh.ocx.kind"] == "launcher")
    inner = next(r for r in records if r["executable"]["sh.ocx.kind"] == "binary")

    assert "/entrypoints/" in outer["process"]["executable"].replace("\\", "/"), (
        "the outer frame resolves the generated launcher under entrypoints/; "
        f"got {outer['process']['executable']}"
    )
    assert "/content/" in inner["process"]["executable"].replace("\\", "/"), (
        "the inner frame resolves the real leaf binary under content/; "
        f"got {inner['process']['executable']}"
    )

    assert _root_digest(outer) == _root_digest(inner), (
        "the two frames join on the package content digest — that join is the "
        "whole reason two records is the right answer; "
        f"outer={_root_digest(outer)} inner={_root_digest(inner)}"
    )

    # The claim lists are JSON arrays, not a comma-joined string. No separator is
    # forbidden in an executable name, so joining would make `["a,b"]` and
    # `["a","b"]` arrive indistinguishable — lossy in the one field that answers
    # "which executables did this package put on PATH".
    annotations = _root_entry(outer)["annotations"]
    assert annotations["sh.ocx.entrypoints"] == ["hello"], (
        f"sh.ocx.entrypoints is a list of names; got {annotations.get('sh.ocx.entrypoints')!r}"
    )


def test_non_entrypoint_invocation_emits_one_binary_record(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """A package declaring no entrypoints records once, as ``binary``.

    The two-record split happens only when a launcher is involved (ADR record
    #2's closing note).
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "plainbin")

    result = ocx.run(
        "package", "exec", published_package.short, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(sink)},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"package exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert record["executable"]["sh.ocx.kind"] == "binary", (
        "a package with no entrypoints resolves straight to the real binary; "
        f"got {record['executable']}"
    )

    # The claim list is a JSON array, not a comma-joined string. No separator is
    # forbidden in an executable name, so joining would make `["a,b"]` and
    # `["a","b"]` arrive indistinguishable — lossy in the one field that answers
    # "which executables did this package put on PATH".
    annotations = _root_entry(record)["annotations"]
    assert annotations["sh.ocx.binaries"] == ["hello"], (
        f"sh.ocx.binaries is a list of names; got {annotations.get('sh.ocx.binaries')!r}"
    )


# ---------------------------------------------------------------------------
# 6. Direct launcher invocation — degraded identity
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_direct_launcher_invocation_records_degraded_identity(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A launcher frame with no ocx parent records truthfully, not completely.

    Package directories are content-shared and carry no registry/repository, so
    logical identity is structurally unrecoverable here (ADR F5/F12). The record
    says so in-band with ``frame.identity == "degraded"`` and emits **no**
    ``uri`` on the package entry rather than fabricating one.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    pkg_root = _package_root(ocx, pkg.short)
    sink = _sink(tmp_path, "degraded")

    result = ocx.run(
        "launcher", "exec", str(pkg_root), "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(sink)},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"launcher exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert record["frame"]["identity"] == "degraded", (
        "a direct launcher invocation cannot recover logical identity and must "
        f"say so; got frame={record['frame']}"
    )
    entry = _root_entry(record)
    assert "uri" not in entry, (
        "no purl can be built without a repository, so the field is omitted "
        f"rather than invented; got {entry}"
    )
    assert BARE_SHA256.match(entry["digest"]["sha256"]), (
        f"the degraded record is still digest-complete; got {entry['digest']}"
    )


# ---------------------------------------------------------------------------
# 7-8. Auto-install — the reporter's actual configuration
# ---------------------------------------------------------------------------


def test_auto_installed_package_is_named_in_resolution(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """Exec of an uninstalled package records the on-the-spot materialisation.

    ``resolution.autoInstalled`` plus ``sh.ocx.resolved-from: "tag"`` are what
    make the drift argument auditable: this invocation resolved a floating tag
    and installed the package right here — state no pull-time record can capture.
    """
    sink = _sink(tmp_path, "autoinstall")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"package exec must auto-install and succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    auto_installed = record["resolution"].get("autoInstalled", [])
    assert any(published_package.repo in entry for entry in auto_installed), (
        "the package materialised by this very invocation must be named in "
        f"resolution.autoInstalled; got {auto_installed}"
    )
    assert _root_entry(record)["annotations"]["sh.ocx.resolved-from"] == "tag", (
        "a user-typed tag was resolved, so the root entry records it; "
        f"got {_root_entry(record)['annotations']}"
    )


def test_already_installed_package_is_not_marked_auto_installed(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """The two drift markers answer different questions and must not move together.

    ``resolution.autoInstalled`` is *invocation*-scoped: did this run materialise
    the package on the spot? It depends on cache state, so it is absent here.

    ``sh.ocx.resolved-from: "tag"`` is *identity*-scoped: was the digest reached
    through a floating tag rather than named directly? That is a property of what
    the user typed, so it holds whether or not this particular run did the
    pulling — a package installed ten minutes ago from a moving tag is still
    tag-derived and still drift-exposed. Tying it to cache state would make the
    drift signal blink in and out between two identical invocations, which is
    exactly the nondeterminism an audit record must not have.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "preinstalled")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"package exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert "autoInstalled" not in record["resolution"], (
        "nothing was installed on this invocation, so the key is absent — not "
        f"an empty list; got resolution={record['resolution']}"
    )
    assert _root_entry(record)["annotations"]["sh.ocx.resolved-from"] == "tag", (
        "the user named a tag, so the identity is tag-derived regardless of the "
        "package already being present; got "
        f"{_root_entry(record)['annotations']}"
    )


# ---------------------------------------------------------------------------
# 9. Warn open — unwritable sink, no policy set
# ---------------------------------------------------------------------------


def test_unwritable_sink_warns_and_runs_when_not_required(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """An unwritable sink with no policy warns; the child still runs, exit 0.

    A developer who fat-fingered ``--records-dir`` once should not have their
    build die for a policy nobody set (ADR posture table, unlocked row).
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _unwritable_sink(tmp_path, "warnopen")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        "an unwritable sink is a warning, not a failure, when no policy is set; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert published_package.marker in result.stdout, (
        f"the child must still run; stdout={result.stdout!r}"
    )
    assert "record" in result.stderr.lower(), (
        f"the failure to record must be visible on stderr; stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# 10. Concurrency — one sink, many invocations
# ---------------------------------------------------------------------------


def test_concurrent_invocations_each_write_a_complete_record(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """N concurrent invocations produce N distinct, individually complete files.

    The directory sink was chosen precisely because it needs no lock: a
    create-exclusive file has no shared offset to contend for (ADR F13). Every
    file must be a whole parseable document — never a partial line.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "concurrent")
    invocations = 8

    def _exec(_: int) -> subprocess.CompletedProcess[str]:
        return ocx.run(
            "package", "exec", "--records-dir", str(sink),
            published_package.short, "--", "hello",
            format=None, check=False,
        )

    with ThreadPoolExecutor(max_workers=invocations) as pool:
        results = list(pool.map(_exec, range(invocations)))

    for index, result in enumerate(results):
        assert result.returncode == EXIT_SUCCESS, (
            f"concurrent invocation {index} failed: rc={result.returncode}\n"
            f"stderr:\n{result.stderr}"
        )

    paths = _record_paths(sink)
    assert len(paths) == invocations, (
        f"{invocations} invocations must leave {invocations} records — one per "
        f"frame, none overwritten; got {len(paths)}: {[p.name for p in paths]}"
    )
    assert len({path.name for path in paths}) == invocations, (
        f"record filenames must be unique; got {[path.name for path in paths]}"
    )

    # _read_records parses every file and enforces the one-line document rule.
    records = _read_records(sink)
    for record in records:
        assert record["kind"] == RECORD_KIND
        assert record["packages"], "every record carries the resolved closure"


# ---------------------------------------------------------------------------
# 11. Fail closed — `required = true` in a config file
# ---------------------------------------------------------------------------


def test_required_true_with_unwritable_sink_exits_74(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``required = true`` + unwritable sink ⇒ exit 74, and the child never runs.

    This is W2's only end-to-end proof. ``required`` is config-file-only — no
    flag, no env var — and the SYSTEM tier is unreachable from pytest, so a
    writable config tier carrying ``required = true`` is the single path to it.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _unwritable_sink(tmp_path, "failclosed")
    _write_home_config(
        ocx,
        f"[records]\ndir = {_toml_path(sink)}\nrequired = true\n",
    )

    result = ocx.run(
        "package", "exec", published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_IO, (
        "an unwritable sink under required = true is a hard stop (exit 74); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert published_package.marker not in result.stdout, (
        "fail-closed means the child NEVER starts — 'approved versions or "
        f"nothing'; stdout={result.stdout!r}"
    )
    assert "record" in result.stderr.lower(), (
        f"the reason must be stated on stderr; stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# 12. Precedence — config ▸ env ▸ CLI
# ---------------------------------------------------------------------------


def test_records_dir_precedence_config_then_env_then_flag(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """Each layer wins over the one below it: config, then env, then the flag."""
    ocx.plain("package", "install", "--select", published_package.short)
    from_config = _sink(tmp_path, "layer-config")
    from_env = _sink(tmp_path, "layer-env")
    from_flag = _sink(tmp_path, "layer-flag")
    _write_home_config(ocx, f"[records]\ndir = {_toml_path(from_config)}\n")

    # Layer 2 only — the config tier is in effect.
    result = ocx.run(
        "package", "exec", published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert len(_record_paths(from_config)) == 1, (
        f"config `dir` must be used when nothing overrides it; "
        f"{from_config} holds {[p.name for p in _record_paths(from_config)]}"
    )

    # Layer 3 — the env var beats the config file.
    result = ocx.run(
        "package", "exec", published_package.short, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(from_env)},
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert len(_record_paths(from_env)) == 1, (
        f"OCX_RECORDS_DIR must beat the config file; {from_env} holds "
        f"{[p.name for p in _record_paths(from_env)]}"
    )
    assert len(_record_paths(from_config)) == 1, (
        "the config sink must not receive a second record once env overrides it"
    )

    # Layer 4 — the flag beats both.
    result = ocx.run(
        "package", "exec", "--records-dir", str(from_flag),
        published_package.short, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(from_env)},
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert len(_record_paths(from_flag)) == 1, (
        f"--records-dir must beat both env and config; {from_flag} holds "
        f"{[p.name for p in _record_paths(from_flag)]}"
    )
    assert len(_record_paths(from_env)) == 1, (
        "the env sink must not receive a second record once the flag overrides it"
    )
    assert len(_record_paths(from_config)) == 1, (
        "the config sink must not receive a third record"
    )


# ---------------------------------------------------------------------------
# 13. Unknown filename placeholder
# ---------------------------------------------------------------------------


def test_unknown_name_placeholder_exits_78(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """An unknown placeholder is a config error at resolve time — exit 78.

    Not 74: the sink is fine, the template is not. The failure mode being
    guarded against is a silently-unexpanded ``{jobid}`` producing a directory
    of identically-named files, discovered during an audit.

    The closed set (``{time}``, ``{pid}``, ``{rand}``, ``{host}``) is exercised
    first, so a template rejected for the wrong reason cannot pass this test.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    accepted_sink = _sink(tmp_path, "template-ok")
    rejected_sink = _sink(tmp_path, "template-bad")

    accepted = ocx.run(
        "package", "exec",
        "--records-dir", str(accepted_sink),
        "--records-name", "{time}-{host}-{pid}-{rand}.json",
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert accepted.returncode == EXIT_SUCCESS, (
        "every placeholder in the closed set must expand; "
        f"rc={accepted.returncode}\nstderr:\n{accepted.stderr}"
    )
    assert len(_record_paths(accepted_sink)) == 1, (
        f"the accepted template must produce a record; sink is "
        f"{[p.name for p in _record_paths(accepted_sink)]}"
    )

    rejected = ocx.run(
        "package", "exec",
        "--records-dir", str(rejected_sink),
        "--records-name", "{jobid}.json",
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert rejected.returncode == EXIT_CONFIG, (
        "an unknown placeholder is a config parse error (exit 78), never a "
        f"silent literal; rc={rejected.returncode}\nstderr:\n{rejected.stderr}"
    )
    assert published_package.marker not in rejected.stdout, (
        "the template is rejected at resolve time, before the child starts; "
        f"stdout={rejected.stdout!r}"
    )
    assert _record_paths(rejected_sink) == [], (
        f"no record may be written under a rejected template; sink holds "
        f"{[p.name for p in _record_paths(rejected_sink)]}"
    )


# ---------------------------------------------------------------------------
# 14. PID semantics — one meaning, two platforms
# ---------------------------------------------------------------------------


def test_record_pid_is_the_process_that_runs_the_tool(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``process.pid`` is the process that runs the tool, on both platforms.

    On Unix ``execvp(2)`` replaces the ocx image, so ocx's own pid *becomes* the
    tool's. On Windows ocx spawns and waits, so the record must carry the
    spawned child's pid — recording ocx's pid there would give one field two
    referents. A consumer never branches on OS.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "pid")

    process = subprocess.Popen(
        [
            str(ocx.binary), "package", "exec",
            "--records-dir", str(sink),
            published_package.short, "--", "hello",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=dict(ocx.env),
    )
    stdout, stderr = process.communicate(timeout=120)
    assert process.returncode == EXIT_SUCCESS, (
        f"package exec must succeed; rc={process.returncode}\nstderr:\n{stderr}"
    )
    assert published_package.marker in stdout, f"child must run; stdout={stdout!r}"

    recorded_pid = _one_record(sink)["process"]["pid"]
    assert isinstance(recorded_pid, int) and recorded_pid > 0, (
        f"process.pid must be a real pid; got {recorded_pid!r}"
    )
    if sys.platform == "win32":
        # Unverified, not coverage: this suite matrices a single ubuntu-latest
        # leg (subsystem-tests.md "Platform Split"), so no CI run has ever
        # executed this branch. It is correct if a Windows leg is added — the
        # Windows record carries `Child::id()`, not ocx's own pid — and it is
        # kept for that day rather than deleted, but it proves nothing today.
        assert recorded_pid != process.pid, (
            "on Windows ocx spawns the tool, so the record carries the SPAWNED "
            f"child's pid, not ocx's ({process.pid})"
        )
    else:
        assert recorded_pid == process.pid, (
            "on Unix ocx execs into the tool, so the recorded pid is the ocx "
            f"process's own; expected {process.pid}, got {recorded_pid}"
        )


# ---------------------------------------------------------------------------
# 15. Best-effort fields never fail the invocation
# ---------------------------------------------------------------------------


def test_record_written_when_every_best_effort_probe_fails(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """Undeterminable environment drops keys — it never fails the invocation.

    Format rule 11: an absent key means "not determinable here"; a present key
    is always true. No ``"unknown"`` sentinel, which would be indistinguishable
    from a host genuinely named ``unknown``. The load-bearing fields are
    untouched, because they come from resolution and not from the environment.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "besteffort")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
        env_overrides={FAIL_PROBES: ",".join(ALL_PROBES)},
    )
    assert result.returncode == EXIT_SUCCESS, (
        "a best-effort probe failure must never fail the invocation; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert published_package.marker in result.stdout, (
        f"the child must still run; stdout={result.stdout!r}"
    )

    record = _one_record(sink)
    host = record.get("host", {})
    assert "name" not in host, f"an undeterminable hostname drops the key; got {host}"
    assert "type" not in record["os"], (
        f"an undeterminable OS family drops the key; got {record['os']}"
    )
    process = record["process"]
    assert "arch" not in process, (
        f"an undeterminable architecture drops the key; got {process}"
    )
    # Both halves of the user block were forced, and the block is emitted only
    # when at least one resolves — so the whole block goes.
    assert "user" not in process, (
        f"an undeterminable user drops the whole block; got {process}"
    )
    assert "parent" not in process, (
        f"an undeterminable parent pid drops the key; got {process}"
    )
    assert "working_directory" not in process, (
        f"an undeterminable working directory drops the key; got {process}"
    )

    assert process["executable"], (
        "process.executable is load-bearing — it comes from resolution, not the "
        "environment, so it cannot be dropped"
    )
    assert process["pid"] > 0, "process.pid is load-bearing and cannot be dropped"
    assert record["packages"], "packages[] is load-bearing and always present"
    assert BARE_SHA256.match(_root_entry(record)["digest"]["sha256"])


def test_unforced_probes_are_present_in_an_ordinary_record(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """The discriminator for the test above: unforced, every key is there.

    Without this, an implementation that simply never emitted the best-effort
    block would pass the omission test — absence proves "not determinable here"
    only if presence is the default on an ordinary host.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "probes-present")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    record = _one_record(sink)
    assert record["host"]["name"], f"a normal host has a name; got {record['host']}"
    assert record["os"]["type"], f"a normal host has an OS family; got {record['os']}"
    process = record["process"]
    assert process["arch"], f"a normal host has an architecture; got {process}"
    assert process["working_directory"], f"a running process has a cwd; got {process}"
    if sys.platform != "win32":
        # Windows has no `getppid` and no uid; both keys are absent there by
        # design, so only the POSIX legs can assert their presence.
        assert process["parent"]["pid"] > 0, f"a POSIX process has a parent; got {process}"
        assert process["user"]["id"], (
            "the effective uid comes from the kernel and always resolves on "
            f"POSIX; got {process.get('user')}"
        )


# ---------------------------------------------------------------------------
# 16. The maintainer-preview exclusion survives the launcher hop
# ---------------------------------------------------------------------------


def _sole_bundle(tmp_path: Path) -> tuple[Path, Path]:
    """The bundle ``make_package_with_entrypoints`` left in ``tmp_path``.

    Returns it with the sidecar ``ocx package create`` wrote beside it — the two
    inputs ``ocx package test`` needs to materialise the same package locally,
    without publishing anything.
    """
    bundles = sorted(tmp_path.glob("bundle-*.tar.xz"))
    assert len(bundles) == 1, f"expected exactly one bundle in {tmp_path}; got {bundles}"
    return bundles[0], resolved_metadata_path(bundles[0])


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher re-entry test")
def test_package_test_of_an_entrypoint_package_writes_no_record(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx package test`` records nothing, entrypoint or not.

    ``package test`` is a maintainer preview over a locally materialised,
    unpublished package: a record from it would describe something that was
    never published, and a collector could not filter it out — the launcher
    frame it would arrive as is indistinguishable from a legitimate direct
    launcher invocation.

    A package declaring an entrypoint re-enters ``ocx launcher exec`` through
    its generated launcher, and that is a **fresh process**: it reads
    ``[records]`` from its own config chain. So both configuration routes are
    exercised — the sink forwarded through the environment, and the sink the
    child re-reads for itself from ``$OCX_HOME/config.toml``. Only an exclusion
    carried by the scratch pkg-root the launcher was baked with closes both.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    bundle, metadata = _sole_bundle(tmp_path)
    sink = _sink(tmp_path, "packagetest")

    forwarded = ocx.run(
        "package", "test",
        "-p", pkg.platform, "-m", str(metadata), "-i", pkg.short, str(bundle),
        "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(sink)},
    )
    assert forwarded.returncode == EXIT_SUCCESS, (
        f"package test must succeed; rc={forwarded.returncode}\nstderr:\n{forwarded.stderr}"
    )
    assert pkg.marker in forwarded.stdout, (
        "the entrypoint must actually have run — otherwise an empty sink proves "
        f"nothing; stdout={forwarded.stdout!r}"
    )
    assert _record_paths(sink) == [], (
        "a maintainer preview must not reach the operator's audit sink; it holds "
        f"{[path.name for path in _record_paths(sink)]}"
    )

    # The same run with the sink in a config file the child re-reads itself, so
    # the exclusion cannot be satisfied merely by not forwarding the env.
    _write_home_config(ocx, f"[records]\ndir = {_toml_path(sink)}\n")
    from_config = ocx.run(
        "package", "test",
        "-p", pkg.platform, "-m", str(metadata), "-i", pkg.short, str(bundle),
        "--", "hello",
        format=None, check=False,
    )
    assert from_config.returncode == EXIT_SUCCESS, (
        f"package test must succeed; rc={from_config.returncode}\nstderr:\n{from_config.stderr}"
    )
    assert pkg.marker in from_config.stdout, (
        f"the entrypoint must actually have run; stdout={from_config.stdout!r}"
    )
    assert _record_paths(sink) == [], (
        "the child re-reads [records] from its own config chain, so the exclusion "
        "must be structural rather than a matter of what the parent forwarded; "
        f"the sink holds {[path.name for path in _record_paths(sink)]}"
    )


# ---------------------------------------------------------------------------
# 17. A symlinked sink is pinned to its target, not refused
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX directory symlink")
def test_sink_designated_through_a_symlink_records_at_its_target(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """A sink that is a symlink when the operator designates it is honoured.

    This test replaces one that asserted the opposite, and the reversal is the
    point. What is guarded is **substitution**, not symlinks: the sink is
    canonicalized and pinned once at policy resolution, so a path that is
    already a link at that moment is a legitimate operator choice and the record
    lands at its target — which is where the operator pointed.

    Refusing it was the wrong guard. Every macOS host reaches an ordinary
    ``/var/log/ocx/records`` through ``/var`` -> ``/private/var``, so the old
    ancestor-walk refusal refused every launch, and under ``required = true``
    refused it permanently.

    The substitution case the guard still covers — a real directory designated
    and pinned, then swapped for a link *before the same process writes* — has no
    acceptance-level subject: pinning and writing happen inside one ocx process,
    with no window a test can open deterministically between them. It is covered
    where it can be: ``crates/ocx_lib/src/record/sink.rs``
    ``a_sink_substituted_after_designation_is_refused``.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    # Resolved, so the designated link is the only one in the chain.
    root = tmp_path.resolve()
    real = root / "real-sink"
    real.mkdir()
    link = root / "records-link"
    link.symlink_to(real, target_is_directory=True)

    result = ocx.run(
        "package", "exec", "--records-dir", str(link),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        "a sink designated through a symlink is an ordinary sink; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert published_package.marker in result.stdout, (
        f"the child must still run; stdout={result.stdout!r}"
    )
    assert len(_record_paths(real)) == 1, (
        "the record lands at the link's target, which is where the operator "
        f"pointed; the target holds {[path.name for path in _record_paths(real)]}"
    )
    assert "symlink" not in result.stderr.lower(), (
        "designating a symlinked sink is not a refusal and must not warn about "
        f"one; stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# 18. The published schema and the serializer, bound to one another
# ---------------------------------------------------------------------------


def test_emitted_record_validates_against_the_published_schema(
    ocx: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
    execution_record_schema: dict,
) -> None:
    """A record ocx actually wrote must validate against the schema ocx publishes.

    The two are otherwise checked independently — ``test_schema_generation.py``
    asserts the schema's shape, this module asserts the record's — so they can
    drift apart with both green and only a consumer would find out. Validating a
    real emitted record against the generated schema is the only assertion that
    fails when they disagree.

    Full validation, deliberately not a hand-rolled subset check: a subset check
    reproduces exactly the drift risk it is supposed to close, because it is a
    third description of the format that can go stale on its own.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "schema")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    record = _one_record(sink)
    validator_class = jsonschema.validators.validator_for(execution_record_schema)
    validator_class.check_schema(execution_record_schema)
    errors = sorted(
        validator_class(execution_record_schema).iter_errors(record),
        key=lambda error: list(error.absolute_path),
    )
    assert not errors, (
        "an emitted record must validate against the published schema; "
        + "\n".join(
            f"  at {'/'.join(str(part) for part in error.absolute_path) or '<root>'}: "
            f"{error.message}"
            for error in errors
        )
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher re-entry test")
def test_launcher_record_validates_against_the_published_schema(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, execution_record_schema: dict
) -> None:
    """The launcher frame's record validates too — a different scope variant.

    ``scope`` is a tagged union and ``resolution`` omits four keys here, so this
    frame exercises schema branches the package-tier record never reaches. One
    frame validating would leave the other two unbound.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    pkg_root = _package_root(ocx, pkg.short)
    sink = _sink(tmp_path, "schema-launcher")

    result = ocx.run(
        "launcher", "exec", str(pkg_root), "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(sink)},
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    record = _one_record(sink)
    assert record["scope"]["tier"] == "launcher"
    validator_class = jsonschema.validators.validator_for(execution_record_schema)
    errors = list(validator_class(execution_record_schema).iter_errors(record))
    assert not errors, (
        "the launcher frame's record must validate against the published schema; "
        + "\n".join(f"  {error.message}" for error in errors)
    )


def test_run_record_validates_against_the_published_schema(
    ocx: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
    execution_record_schema: dict,
) -> None:
    """The project frame's record validates too — the third scope variant.

    Its ``scope`` carries the lock reference and the group list, which neither
    other frame has.
    """
    project = _project_with_tool(ocx, tmp_path, published_package)
    sink = _sink(tmp_path, "schema-run")

    result = _run_in(ocx, project, "exec", "--records-dir", str(sink), "--", "hello")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    record = _one_record(sink)
    assert record["scope"]["tier"] == "project"
    validator_class = jsonschema.validators.validator_for(execution_record_schema)
    errors = list(validator_class(execution_record_schema).iter_errors(record))
    assert not errors, (
        "the project frame's record must validate against the published schema; "
        + "\n".join(f"  {error.message}" for error in errors)
    )


# ---------------------------------------------------------------------------
# 19. The record's file mode
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX permission bits")
def test_record_file_is_readable_only_by_its_owner(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """A published record is mode 0600, not umask-derived.

    Records carry absolute paths including the invoking user's home directory,
    the invoking user's own name and uid, and the full resolved closure. The sink
    is routinely a shared directory on a multi-tenant host, so a regression to a
    umask-derived 0644 would make every job's record world-readable — and nothing
    else in the suite would notice, because the record would still be there and
    still parse.

    0600 is what ``NamedTempFile`` creates and what the no-clobber publish
    preserves; this pins it as a property of the format rather than an accident
    of the primitive.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "mode")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    paths = _record_paths(sink)
    assert len(paths) == 1, f"expected one record; got {[p.name for p in paths]}"
    mode = os.stat(paths[0]).st_mode & 0o777
    assert mode == 0o600, (
        "a record is readable and writable by its owner and nobody else; got "
        f"{mode:#o} on {paths[0].name}"
    )


# ---------------------------------------------------------------------------
# 20. The flag tier is the one a child cannot re-derive
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher re-entry test")
def test_records_dir_flag_alone_reaches_the_launcher_re_entry(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--records-dir`` alone must still produce BOTH halves of the pair.

    This is the only test that can prove the flag survives the launcher hop. The
    sibling entrypoint tests set ``OCX_RECORDS_DIR``, so the re-entry re-derives
    the same sink from its own environment tier and they would stay green with
    flag forwarding deleted entirely.

    The config and environment tiers a child re-reads for itself. The CLI tier is
    per-invocation and ``apply_ocx_config`` is set-**or-remove**, so without the
    forward the inner frame records somewhere else — or, more likely, nowhere —
    and the entrypoint pair silently loses the leaf binary, which is the one
    field the inner record exists to carry.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    sink = _sink(tmp_path, "flag-forward")

    assert "OCX_RECORDS_DIR" not in ocx.env, (
        "the whole point of this test is that the flag is the ONLY channel; a "
        "sink in the environment would let the child re-derive it"
    )
    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        pkg.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"entrypoint invocation must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    records = _read_records(sink)
    kinds = sorted(record["executable"]["sh.ocx.kind"] for record in records)
    assert kinds == ["binary", "launcher"], (
        "the flag tier must be forwarded across the launcher hop, or the inner "
        f"frame records nowhere and the leaf binary is lost; got {kinds}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher re-entry test")
def test_records_name_flag_alone_reaches_the_launcher_re_entry(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--records-name`` is forwarded too, for the same reason as the sink.

    A collector that globs or parses filenames depends on the pattern as much as
    on the directory, so a re-entry falling back to the default grammar would
    break collection in exactly the half of the pair nobody is watching.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    sink = _sink(tmp_path, "flag-forward-name")

    result = ocx.run(
        "package", "exec",
        "--records-dir", str(sink),
        "--records-name", "rec-{time}-{pid}-{rand}.json",
        pkg.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    names = [path.name for path in _record_paths(sink)]
    assert len(names) == 2, f"an entrypoint invocation records twice; got {names}"
    assert all(name.startswith("rec-") for name in names), (
        "both frames must render the template the outer frame resolved; got "
        f"{names}"
    )


# ---------------------------------------------------------------------------
# 21-23. The child's exit status is the invocation's exit status
# ---------------------------------------------------------------------------


def test_child_exit_code_is_propagated(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``ocx package exec`` exits with the child's status, not its own.

    The record answers "what was about to run", never "what happened" — the ADR
    is explicit that callers correlate ``$?`` themselves. That makes the exit
    status the *only* channel carrying the result, and nothing at any level
    asserted it.
    """
    ocx.plain("package", "install", "--select", published_package.short)

    result = ocx.run(
        "package", "exec", published_package.short, "--", "sh", "-c", "exit 7",
        format=None, check=False,
    )
    assert result.returncode == 7, (
        "the child's exit status is the invocation's exit status; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX signal semantics")
def test_signalled_child_reports_the_signal(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """A child killed by a signal dies of that signal, not of an exit status.

    On Unix ``execvp`` replaces the ocx image, so the parent reaps the tool's own
    death directly: ``wait(2)`` reports "terminated by signal 9", which Python
    surfaces as ``-9`` and a shell renders as ``137``. Pinned because a change to
    spawn-and-wait on this path would flatten that into an ordinary exit status,
    and a caller keying on 137 would stop noticing that its job was OOM-killed.
    """
    ocx.plain("package", "install", "--select", published_package.short)

    result = ocx.run(
        "package", "exec", published_package.short,
        "--", "sh", "-c", f"kill -{int(signal.SIGKILL)} $$",
        format=None, check=False,
    )
    assert result.returncode == -int(signal.SIGKILL), (
        "a SIGKILLed child must be reaped as signal-terminated (Python -9, "
        f"shell 137), never as a plain exit status; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


def test_recording_run_still_propagates_the_child_exit_code(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """Writing a record must not swallow or replace the child's status.

    The recording path adds a serialize and a file write immediately before the
    launch, and it has its own failure exit (74). Nothing else proves that a
    *successful* record leaves the child's own status alone — an implementation
    that returned its own success would pass every other test in this module.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    sink = _sink(tmp_path, "exitcode")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "sh", "-c", "exit 7",
        format=None, check=False,
    )
    assert result.returncode == 7, (
        "recording must not replace the child's exit status; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert len(_record_paths(sink)) == 1, (
        "the record still lands — a failing child is not a failing record; sink "
        f"holds {[path.name for path in _record_paths(sink)]}"
    )


# ---------------------------------------------------------------------------
# 24. Multi-package attribution
# ---------------------------------------------------------------------------


def test_multi_package_exec_names_only_the_absent_package_as_auto_installed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """With two roots, ``autoInstalled`` names the one this run materialized.

    ``autoInstalled`` is built by zipping the requested identifiers against the
    resolution results positionally, and every other record test names exactly
    one package — so the zip has never run against a case where it could be
    misaligned. Two roots with *different* materialization states is the smallest
    case that discriminates: mis-zipped, the record would credit the installation
    to the package that was already present, which is the drift signal pointing
    at the wrong artefact.
    """
    present = make_package(ocx, f"{unique_repo}_present", "1.0.0", tmp_path / "present")
    absent = make_package(ocx, f"{unique_repo}_absent", "1.0.0", tmp_path / "absent")
    ocx.plain("package", "install", "--select", present.short)
    sink = _sink(tmp_path, "multipkg")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        present.short, absent.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"a two-package exec must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    auto_installed = record["resolution"].get("autoInstalled", [])
    assert len(auto_installed) == 1, (
        "exactly one of the two roots was materialized by this invocation; got "
        f"{auto_installed}"
    )
    assert absent.repo in auto_installed[0], (
        "autoInstalled must name the package this run pulled, never the one that "
        f"was already installed ({present.repo}); got {auto_installed}"
    )
    assert present.repo not in auto_installed[0], (
        "a positional mis-zip would credit the installation to the wrong root; "
        f"got {auto_installed}"
    )

    roots = {
        entry["name"]
        for entry in _entries_with_role(record, "root")
    }
    assert roots == {present.repo, absent.repo}, (
        "both packages named on the command line are roots of this composition; "
        f"got {sorted(roots)}"
    )


# ---------------------------------------------------------------------------
# 25-26. Format rule 5 — the platform leaf, never the index
# ---------------------------------------------------------------------------


def test_root_digest_is_the_platform_leaf_never_the_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The recorded digest names the exact bits that ran.

    Format rule 5, and the one rule with a decade of precedent behind it:
    Kubernetes' ``imageID`` has been unwinding index-vs-platform-manifest
    confusion since 2015. A record naming the multi-arch index would say only
    "some build of this version", which is not an audit answer.

    A single-platform fixture cannot discriminate — its index has one child and
    both digests are one hop apart but a wrong implementation looks identical for
    any *other* reason. So the fixture ships two platforms, and the assertion is
    both halves: equal to the host's leaf, **and** different from the index.
    """
    host_platform = current_platform()
    foreign = "linux/arm64" if host_platform != "linux/arm64" else "linux/amd64"
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "host", platform=host_platform)
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "foreign", platform=foreign)

    index_digest = fetch_manifest_digest(ocx.registry, unique_repo, "1.0.0")
    leaf_digest = fetch_platform_manifest_digest(
        ocx.registry, unique_repo, "1.0.0", platform=host_platform
    )
    assert leaf_digest != index_digest, (
        "the fixture must be genuinely multi-platform, or this test cannot fail; "
        f"index={index_digest} leaf={leaf_digest}"
    )

    sink = _sink(tmp_path, "leafdigest")
    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        f"{unique_repo}:1.0.0", "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"exec of the multi-platform package must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    recorded = _root_digest(_one_record(sink))
    assert f"sha256:{recorded}" == leaf_digest, (
        "the recorded digest is the platform LEAF manifest digest; expected "
        f"{leaf_digest}, got sha256:{recorded}"
    )
    assert f"sha256:{recorded}" != index_digest, (
        "the recorded digest must never be the multi-arch index digest — that "
        f"names the pointer, not the bits; got {index_digest}"
    )


def test_cached_multi_platform_root_records_the_platform_it_selected(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A root resolved *from the store* still reports the platform it selected.

    Every other platform assertion in this file runs against a package this very
    invocation pulled, so all of them pass whether or not the cached path records
    anything: the pull path stamps the resolved platform on its way through, and
    a package already in the store never goes that way. An audit that says
    ``linux/amd64`` on the first run of a job and omits the platform on every
    later run is not an audit trail — the fact must not depend on cache state.

    The fixture ships two platforms so the recorded value is a *selection* rather
    than the only thing on offer, and the assertion is both carriers of it: the
    ``sh.ocx.platform`` annotation and the purl's ``arch`` qualifier.
    """
    host_platform = current_platform()
    foreign = "linux/arm64" if host_platform != "linux/arm64" else "linux/amd64"
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "host", platform=host_platform)
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "foreign", platform=foreign)

    # Install first: the recorded invocation below must resolve from the store,
    # which is the path under test.
    ocx.plain("package", "install", "--select", f"{unique_repo}:1.0.0")
    sink = _sink(tmp_path, "cachedplatform")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        f"{unique_repo}:1.0.0", "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"exec of the pre-installed multi-platform package must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert "autoInstalled" not in record["resolution"], (
        "this invocation must have resolved from the store, or it exercises the "
        f"pull path instead of the cached one; got resolution={record['resolution']}"
    )

    assert record["resolution"]["registries"] == [ocx.registry], (
        "the content registry is stamped by the same resolution, so the cached "
        f"path must report it too; got {record['resolution'].get('registries')!r}"
    )

    root = _root_entry(record)
    assert root["annotations"].get("sh.ocx.platform", "").startswith(host_platform), (
        "a root resolved from the store records the platform its resolution "
        f"selected; got {root['annotations']}"
    )
    assert _purl_qualifiers(root["uri"]).get("arch") == [host_platform.split("/")[1]], (
        "the purl's arch qualifier carries the same selection; got "
        f"{root['uri']!r}"
    )


def test_dependency_entries_carry_no_platform_and_no_arch_qualifier(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``sh.ocx.platform`` is a root-only annotation, and so is the purl's ``arch``.

    A dependency is reachable at record time as an identifier, not as an install,
    so the platform it *selected* is not in hand — and the frame's requested
    platform is a different fact. Annotating it anyway would make the record
    state something it does not know, in the field an auditor reads to decide
    which bits ran. The purl's ``arch`` qualifier follows the same rule: it is
    truthful on a root and decorative on a dependency.
    """
    dependency = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path / "dep")
    dependency_digest = fetch_platform_manifest_digest(
        ocx.registry, dependency.repo, dependency.tag
    )
    root = make_package(
        ocx,
        f"{unique_repo}_root",
        "1.0.0",
        tmp_path / "root",
        dependencies=[{"identifier": f"{dependency.fq}@{dependency_digest}"}],
    )
    sink = _sink(tmp_path, "closure")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        root.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"exec with a dependency closure must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    roots = _entries_with_role(record, "root")
    dependencies = _entries_with_role(record, "dependency")
    assert len(roots) == 1, f"one root was named; got {[e['name'] for e in roots]}"
    assert [entry["name"] for entry in dependencies] == [dependency.repo], (
        "the closure carries the declared dependency; got "
        f"{[entry['name'] for entry in dependencies]}"
    )

    root_annotations = roots[0]["annotations"]
    assert root_annotations["sh.ocx.platform"].startswith(current_platform()), (
        "a root records the platform its resolution selected; got "
        f"{root_annotations.get('sh.ocx.platform')!r}"
    )
    assert _purl_qualifiers(roots[0]["uri"]).get("arch") == [
        current_platform().split("/")[1]
    ], (
        "a root's purl carries a truthful arch qualifier; got "
        f"{roots[0]['uri']!r}"
    )

    dependency_annotations = dependencies[0]["annotations"]
    assert "sh.ocx.platform" not in dependency_annotations, (
        "a dependency's selected platform is not in hand, so the key is omitted "
        f"rather than guessed; got {dependency_annotations}"
    )
    assert "arch" not in _purl_qualifiers(dependencies[0]["uri"]), (
        "a dependency's purl carries no arch qualifier for the same reason; got "
        f"{dependencies[0]['uri']!r}"
    )
    assert BARE_SHA256.match(dependencies[0]["digest"]["sha256"]), (
        "a dependency is still digest-complete; got "
        f"{dependencies[0]['digest']}"
    )


# ---------------------------------------------------------------------------
# 28. Index indirection — the host the bytes came from
# ---------------------------------------------------------------------------


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    """A local ``index.ocx.sh``-shaped static fixture.

    Mirrors the fixture of the same name in ``test_index_ocx_sh.py``, which owns
    the wire-shape coverage; here it exists only to make the logical namespace
    and the physical registry differ, which no plain-OCI fixture can do.
    """
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


def test_registries_name_the_content_host_not_the_logical_namespace(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """``resolution.registries`` is where the bytes came from, not what names them.

    Under index indirection an identifier's registry is the *logical* namespace:
    ``ocx.sh/<repo>/pkg`` resolves through the index to a physical pointer and
    every byte is fetched from there. Every other test in this file publishes
    plain OCI, where the two hosts are the same string — so none of them can tell
    a field built from the identifier apart from one built from the transport,
    and a record could name a host that served nothing without a single test
    turning red.

    Both facts are recorded, in different places, and this asserts each in its
    own: the content host under ``resolution.registries``, the logical namespace
    under the root purl's ``repository_url``.
    """
    package = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, package.repo, package.tag)
    os_name, arch_name = package.platform.split("/")

    # `[registries."ocx.sh"] index = <fixture>` plus the loopback escape hatch
    # the read-path SSRF guard needs — same configuration as the fixture's home
    # module, `test_index_ocx_sh.py::configure_index_source`.
    _write_home_config(
        ocx,
        f'[registries."ocx.sh"]\n'
        f'index = "{index_server.base_url}"\n'
        f'trusted_hosts = ["{ocx.registry.split(":", 1)[0]}"]\n',
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"
    static_index.write_config(index_server.root)
    entry = static_index.write_package(
        index_server.root,
        repository=f"{unique_repo}/pkg",
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{package.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    sink = _sink(tmp_path, "indirected")

    result = ocx.run(
        "--index", str(index_dir),
        "package", "exec", "--records-dir", str(sink),
        entry.logical_id, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"exec through the index fixture must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert record["resolution"]["registries"] == [ocx.registry], (
        "resolution.registries names the registry the content was fetched from "
        f"({ocx.registry}), never the logical namespace (ocx.sh) the identifier "
        f"is named by; got {record['resolution'].get('registries')!r}"
    )
    assert _purl_qualifiers(_root_entry(record)["uri"])["repository_url"] == [
        f"ocx.sh/{unique_repo}/pkg"
    ], (
        "the logical namespace is not lost either — it is what the purl carries, "
        "as the registry plus the full repository the `oci` purl type specifies; "
        f"got {_root_entry(record)['uri']!r}"
    )


# ---------------------------------------------------------------------------
# 29-31. Three ways the fail-closed guarantee used to be false
# ---------------------------------------------------------------------------
#
# "No caller can opt out of a sink the operator has locked at system scope" is
# this feature's headline claim. Each test below covers one path that made it
# false while every other test in this file stayed green.


def test_required_true_with_no_sink_is_a_configuration_error(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """``required = true`` and no ``dir`` anywhere exits 78, and nothing runs.

    A ``[records]`` block carrying nothing but ``required = true`` is the
    plainest way an operator writes "recording is mandatory". It used to resolve
    to a policy with no sink, which the launch path skips *before* the posture is
    ever applied — so every child on every host ran unrecorded, exit 0, no
    warning. Exit 78 rather than 74: the operator fixes it by adding a ``dir``,
    not by clearing an I/O condition.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    _write_home_config(ocx, "[records]\nrequired = true\n")

    result = ocx.run(
        "package", "exec", published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_CONFIG, (
        "a fail-closed posture with nothing to write to is a configuration error, "
        f"not a silent opt-out; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert published_package.marker not in result.stdout, (
        f"the refusal precedes the child; stdout={result.stdout!r}"
    )


def test_a_symlinked_system_config_refuses_the_launch(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """An unusable SYSTEM config aborts rather than dropping operator policy.

    The SYSTEM tier is where a locked ``[records]`` policy lives, and it used to
    be filtered with the same best-effort semantics as the user tiers: a symlink
    was skipped with a warning and the launch proceeded with no ``[records]``
    block at all. Symlinking ``/etc/ocx/config.toml`` at a fleet-managed file is
    an ordinary config-management move, and it silently took the whole fleet out
    of recording.

    Reached through ``__OCX_TESTING_SYSTEM_CONFIG``, the test seam that redirects
    the SYSTEM path — pytest cannot write ``/etc``.
    """
    ocx.plain("package", "install", "--select", published_package.short)
    fleet = tmp_path / "fleet-config.toml"
    fleet.write_text(f"[records]\ndir = {_toml_path(_sink(tmp_path, 'fleet'))}\n")
    system = tmp_path / "system-config.toml"
    system.symlink_to(fleet)

    result = ocx.run(
        "package", "exec", published_package.short, "--", "hello",
        format=None, check=False,
        env_overrides={"__OCX_TESTING_SYSTEM_CONFIG": str(system)},
    )
    assert result.returncode == EXIT_CONFIG, (
        "a SYSTEM config that exists and cannot be consulted is fatal, never a "
        f"warning; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert published_package.marker not in result.stdout, (
        f"the refusal precedes the child; stdout={result.stdout!r}"
    )

    # The discriminator: the same seam pointed at a real file loads it and runs,
    # so what refuses above is the symlink and not the seam itself.
    system.unlink()
    system.write_text(fleet.read_text())
    ok = ocx.run(
        "package", "exec", published_package.short, "--", "hello",
        format=None, check=False,
        env_overrides={"__OCX_TESTING_SYSTEM_CONFIG": str(system)},
    )
    assert ok.returncode == EXIT_SUCCESS, (
        f"a regular SYSTEM config still loads; rc={ok.returncode}\nstderr:\n{ok.stderr}"
    )
    assert published_package.marker in ok.stdout, (
        f"and the child runs; stdout={ok.stdout!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_a_forged_scratch_pkg_root_cannot_escape_a_required_policy(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """A launcher exemption claimed by path placement is refused under a policy.

    ``ocx launcher exec`` grants the maintainer-preview exemption when the
    caller-supplied pkg-root sits under ``$OCX_HOME/temp/test/`` — a directory
    inside the invoking user's own home, and ``launcher exec`` is a
    hidden-but-invocable wire ABI. Copying an installed package tree there is the
    whole forgery, and the exempt path used to skip the ``[records]`` fold
    entirely, so an operator's ``required = true`` was never consulted.

    A capability token cannot close this — parent and forger run as the same uid
    — so the exemption is bounded by the one thing the caller does not control:
    under a fail-closed posture no exemption is granted and the launch is
    refused (exit 74).
    """
    ocx.plain("package", "install", "--select", published_package.short)
    pkg_root = _package_root(ocx, published_package.short)

    forged = Path(ocx.env["OCX_HOME"]) / "temp" / "test" / "forged-pkg"
    forged.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(pkg_root, forged, symlinks=True)

    sink = _sink(tmp_path, "forged")
    _write_home_config(
        ocx,
        f"[records]\ndir = {_toml_path(sink)}\nrequired = true\n",
    )

    result = ocx.run(
        "launcher", "exec", str(forged), "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_IO, (
        "a fail-closed posture and an exemption are a contradiction, resolved in "
        f"the operator's favour; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert published_package.marker not in result.stdout, (
        "the tool must not have run — an exempt launch that runs is exactly the "
        f"opt-out the policy forbids; stdout={result.stdout!r}"
    )
    assert _record_paths(sink) == [], (
        "and the refusal is a refusal, not a silent downgrade to recording the "
        f"preview; the sink holds {[path.name for path in _record_paths(sink)]}"
    )
    assert "records" in result.stderr.lower(), (
        f"the policy that refused it must be named; stderr={result.stderr!r}"
    )

    # The discriminator: the same forged root under a warn-only policy still
    # runs, so what refuses above is the posture and not the copy.
    _write_home_config(ocx, f"[records]\ndir = {_toml_path(sink)}\n")
    warn_only = ocx.run(
        "launcher", "exec", str(forged), "--", "hello",
        format=None, check=False,
    )
    assert warn_only.returncode == EXIT_SUCCESS, (
        f"an unbounded posture grants the exemption; rc={warn_only.returncode}\n"
        f"stderr:\n{warn_only.stderr}"
    )
    assert published_package.marker in warn_only.stdout, (
        f"the preview runs; stdout={warn_only.stdout!r}"
    )
    assert _record_paths(sink) == [], (
        "and it still records nothing — the exemption is bounded, not deleted; "
        f"the sink holds {[path.name for path in _record_paths(sink)]}"
    )
# ---------------------------------------------------------------------------
# 32. Transport — which registries were reachable in the clear
# ---------------------------------------------------------------------------


def test_a_plain_http_registry_is_named_in_insecure_registries(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``resolution.insecureRegistries`` is the fetched-AND-exempt intersection.

    The test registry is plain HTTP, declared through
    ``OCX_INSECURE_REGISTRIES`` by the runner itself, so an invocation that
    fetches from it names it. Both states are asserted from one record: a second
    host is declared exempt and never contacted, and must NOT appear — a record
    that echoed the configured allowance would say nothing about the invocation,
    which is the whole point of the field.
    """
    sink = _sink(tmp_path, "insecure")
    unused = "unused.invalid:5000"

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_INSECURE_REGISTRIES": f"{ocx.registry},{unused}"},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"package exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    assert record["resolution"]["registries"] == [ocx.registry], (
        "the frame fetched from exactly one host; got "
        f"{record['resolution'].get('registries')!r}"
    )
    assert record["resolution"]["insecureRegistries"] == [ocx.registry], (
        "the host this frame fetched from over plain HTTP is named, and the "
        "declared-but-uncontacted one is not; got "
        f"{record['resolution'].get('insecureRegistries')!r}"
    )
    assert unused not in json.dumps(record), (
        "a configured allowance nothing was fetched from must not reach the "
        f"record at all; got {record['resolution']}"
    )


# ---------------------------------------------------------------------------
# 33. Patch tier — companions in the closure, snapshot in the resolution
# ---------------------------------------------------------------------------


def _publish_companion_for(
    ocx: OcxRunner, base: PackageInfo, companion_repo: str, tmp_path: Path
) -> str:
    """Publish an env-only companion and a per-base rule admitting it.

    Per-base rather than ``--global``: the global descriptor slot is one
    registry-wide repository that ``test_patches.py`` serializes access to, and
    this module must not join that contention. ``required = false`` so a rule a
    concurrent module left in the global slot fails open here instead of failing
    this test closed.
    """
    companion = make_package(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path / "companion",
        bins=[],
        env=[
            {
                "key": "RECORDED_COMPANION_VAR",
                "type": "constant",
                "value": "companion-value",
                "visibility": "interface",
            }
        ],
        cascade=True,
        platform="any",
    )
    _write_home_config(
        ocx, f'[patches]\nregistry = "{ocx.registry}"\nrequired = false\n'
    )
    descriptor = tmp_path / "record_descriptor.json"
    descriptor.write_text(
        json.dumps(
            {"version": 1, "rules": [{"match": "*", "packages": [companion.fq]}]}
        )
    )
    publish = ocx.run(
        "patch", "publish", "--descriptor", str(descriptor), base.fq,
        format=None, check=False,
    )
    assert publish.returncode == EXIT_SUCCESS, (
        f"patch publish must succeed; rc={publish.returncode}\n"
        f"stderr:\n{publish.stderr}"
    )
    return companion.fq


def _companion_entry(record: dict, repo: str) -> dict:
    """The ``sh.ocx.role: companion`` entry for ``repo``."""
    companions = _entries_with_role(record, "companion")
    matching = [entry for entry in companions if entry["name"] == repo]
    assert matching, (
        f"no companion entry named {repo!r}; the record carries "
        f"{[entry['name'] for entry in companions]}"
    )
    return matching[0]


def test_a_patched_invocation_records_its_companion_and_snapshot(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A patched exec names the companion, and a frozen one names the snapshot.

    Two invocations of the same composition, so each of the two facts is
    asserted in both states from data this test controls:

    * the companion entry is present in both, because site policy applied in
      both — it is not a property of the freeze;
    * ``resolution.patchSnapshot`` is present only under
      ``OCX_PATCH_SNAPSHOT``, and carries the digest of that file's own bytes,
      so a record cannot claim a freeze that was not in force.
    """
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path / "base", cascade=True)
    companion_repo = f"{unique_repo}_companion"
    _publish_companion_for(ocx, base, companion_repo, tmp_path)

    install = ocx.run("package", "install", base.short, format=None, check=False)
    assert install.returncode == EXIT_SUCCESS, (
        f"install must discover and fetch the companion; rc={install.returncode}\n"
        f"stderr:\n{install.stderr}"
    )

    # ── Live patch tier, no snapshot ──
    live_sink = _sink(tmp_path, "patched-live")
    live = ocx.run(
        "package", "exec", "--records-dir", str(live_sink),
        base.short, "--", "hello",
        format=None, check=False,
    )
    assert live.returncode == EXIT_SUCCESS, (
        f"the patched exec must succeed; rc={live.returncode}\n"
        f"stderr:\n{live.stderr}"
    )
    live_record = _one_record(live_sink)
    companion = _companion_entry(live_record, companion_repo)
    qualifiers = _purl_qualifiers(companion["uri"])
    assert qualifiers.get("repository_url") == [f"{ocx.registry}/{companion_repo}"], (
        "a companion is described by an ordinary purl, like every other entry — "
        "registry plus the full repository, per the `oci` purl type; "
        f"got {companion['uri']!r}"
    )
    assert qualifiers.get("tag") == ["1.0.0"], (
        "the tag the descriptor rule named survives as the purl's own qualifier; "
        f"got {companion['uri']!r}"
    )
    assert BARE_SHA256.match(companion["digest"]["sha256"]), (
        f"a companion is digest-complete like every other entry; got {companion['digest']}"
    )
    assert companion["annotations"]["sh.ocx.visibility"] == "interface", (
        "the overlay composes a companion's interface surface and nothing else; "
        f"got {companion['annotations']}"
    )
    # A ``--global`` rule ``test_patches.py`` has live in the registry-wide slot
    # composes a second companion here; that is the site tier working, not a
    # defect. The claim is placement: every companion follows everything the
    # caller asked for, however many the site tier overlaid.
    roles = [entry["annotations"]["sh.ocx.role"] for entry in live_record["packages"]]
    first_companion = roles.index("companion")
    assert all(role == "companion" for role in roles[first_companion:]), (
        "site policy is recorded after everything the caller asked for; got "
        f"{roles}"
    )
    assert "patchSnapshot" not in live_record["resolution"], (
        "no snapshot was in force, so the key is absent — not an empty object; "
        f"got resolution={live_record['resolution']}"
    )

    # ── Same composition under a freeze ──
    freeze = ocx.run("--global", "patch", "freeze", format="json", check=False)
    assert freeze.returncode == EXIT_SUCCESS, (
        f"patch freeze must succeed; rc={freeze.returncode}\nstderr:\n{freeze.stderr}"
    )
    snapshot_path = Path(json.loads(freeze.stdout)["path"])
    assert snapshot_path.is_file(), f"freeze must write {snapshot_path}"

    frozen_sink = _sink(tmp_path, "patched-frozen")
    frozen = ocx.run(
        "package", "exec", "--records-dir", str(frozen_sink),
        base.short, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_PATCH_SNAPSHOT": str(snapshot_path)},
    )
    assert frozen.returncode == EXIT_SUCCESS, (
        f"the frozen exec must succeed; rc={frozen.returncode}\n"
        f"stderr:\n{frozen.stderr}"
    )
    frozen_record = _one_record(frozen_sink)
    assert frozen_record["resolution"]["patchSnapshot"] == {
        "sha256": hashlib.sha256(snapshot_path.read_bytes()).hexdigest()
    }, (
        "the recorded digest is over the snapshot file's own bytes, so it moves "
        "when the pins do; got "
        f"{frozen_record['resolution'].get('patchSnapshot')!r}"
    )
    assert _companion_entry(frozen_record, companion_repo)["digest"] == companion["digest"], (
        "the freeze pinned what was already composed, so the companion digest is "
        "unchanged — the snapshot key is the only difference between the two runs"
    )
