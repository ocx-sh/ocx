# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The vendored launcher starts tools from a moved, read-only or runfiles tree (design § 4.2).

The launcher is a copy of one blob plus a `NAME.launch` record; nothing else may
be needed to start a tool. Each case runs the launcher with no `ocx` on `PATH`
and carries its control: the same setup shown failing, or the observation
shown able to see what the case asserts is absent.
"""

from __future__ import annotations

import json
import os
import platform
import shlex
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
from pathlib import Path
from typing import Any
from uuid import uuid4

import pytest

from src.runner import OcxRunner, PackageInfo, current_platform
from src.vendor_fixtures import VENDOR_TOKEN, entrypoint_package, publish_tree

pytestmark = pytest.mark.command("vendor")

HOST = ("-p", current_platform())
posix_only = pytest.mark.skipif(
    sys.platform == "win32", reason="POSIX shell tools, FIFOs and modes"
)
windows_only = pytest.mark.skipif(
    sys.platform != "win32", reason="Windows batch files and environment case folding"
)
darwin_only = pytest.mark.skipif(
    sys.platform != "darwin", reason="Mach-O code signatures exist on macOS only"
)
PATH_ENTRY = {
    "key": "PATH",
    "type": "path",
    "value": "${installPath}/bin",
    "visibility": "public",
}
DATA_ENTRY = {
    "key": "DATA_FILE",
    "type": "constant",
    "value": "${installPath}/share/data",
    "visibility": "public",
}
CLEAN_PATH = "/usr/bin:/bin"
EX_CONFIG = 78

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run(
    argv: list[str],
    *,
    cwd: Path | str,
    env: dict[str, str],
    timeout: float = 30,
    executable: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `argv` with exactly `env` (no `ocx` on `PATH`); a hang fails the test."""
    try:
        return subprocess.run(
            argv,
            cwd=cwd,
            env=env,
            executable=str(executable) if executable else None,
            capture_output=True,
            encoding="utf-8",
            stdin=subprocess.DEVNULL,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        pytest.fail(f"{shlex.join(argv)} did not return within {timeout}s")


def _ocx(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        [str(ocx.binary), "--format", "json", *args],
        cwd=cwd,
        capture_output=True,
        encoding="utf-8",
        env=ocx.env,
        stdin=subprocess.DEVNULL,
        check=False,
    )
    if result.returncode != 0:
        raise AssertionError(
            f"{shlex.join(args)} failed (rc={result.returncode})\nstderr: {result.stderr.strip()}"
        )
    return result


def _vendor_project(
    ocx: OcxRunner, root: Path, tools: dict[str, PackageInfo]
) -> tuple[Path, dict[str, Any]]:
    """Lock a project of `tools`, vendor it for the host; return the output dir and its manifest."""
    root.mkdir(parents=True)
    (root / "ocx.toml").write_text(
        "[tools]\n" + "".join(f'{name} = "{pkg.fq}"\n' for name, pkg in tools.items()),
        encoding="utf-8",
    )
    _ocx(ocx, root, "lock", "--no-pull")
    out = root.parent / f"out-{uuid4().hex[:8]}"
    _ocx(ocx, root, "vendor", "-o", str(out), *HOST)
    return out, json.loads((out / "manifest.json").read_text(encoding="utf-8"))


def _launcher(out: Path, manifest: dict[str, Any], name: str) -> Path:
    (tool,) = [t for t in manifest["tools"] if t["name"] == name]
    return out / tool["launcher_path"]


def _record(launcher: Path) -> Path:
    stem = launcher.name.removesuffix(".exe")
    return launcher.with_name(f"{stem}.launch")


def _script_package(
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    tools: dict[str, str],
    env: list[dict[str, Any]] | None = None,
) -> PackageInfo:
    """A package whose `bin/<name>` are the given `#!/bin/sh` bodies, plus `share/data`."""

    def build(root: Path) -> None:
        (root / "bin").mkdir()
        (root / "share").mkdir()
        (root / "share" / "data").write_text("payload\n", encoding="utf-8")
        for name, body in tools.items():
            script = root / "bin" / name
            script.write_text(f"#!/bin/sh\n{body}\n", encoding="utf-8")
            script.chmod(0o755)

    return publish_tree(
        ocx,
        repo,
        "1.0.0",
        tmp_path,
        build,
        {"env": env if env is not None else [PATH_ENTRY, DATA_ENTRY]},
    )


# `cd /` first: a root that was left relative breaks the next line, and the script then exits 3
# (the last command's status would otherwise hide the failed `cat`).
TOOL_BODY = 'cd /\ncat "$DATA_FILE" || exit 3\necho "ran:$0"'


@pytest.fixture()
def vendored_tool(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> tuple[Path, dict[str, Any]]:
    pkg = _script_package(ocx, unique_repo, tmp_path / "pkg", {"tool": TOOL_BODY})
    return _vendor_project(ocx, tmp_path / "proj", {"tool": pkg})


def _stage_runfiles(
    parent: Path, out: Path, launcher: Path, *, repo: str = "repo"
) -> Path:
    """`<parent>/tool` + `tool.launch` + `tool.runfiles/<repo>/<whole vendored tree>`, as Bazel stages a tool."""
    parent.mkdir(parents=True)
    started = parent / "tool"
    shutil.copy2(launcher, started)
    shutil.copy2(_record(launcher), parent / "tool.launch")
    shutil.copytree(out, parent / "tool.runfiles" / repo, symlinks=True)
    return started


def _static_exit_elf(code: int) -> bytes:
    """A static ELF exiting `code` with no libc and no loader: runs where no shell exists."""
    arch = platform.machine().lower()
    if arch in ("x86_64", "amd64"):
        machine, body = 62, bytes([0xBF, code, 0, 0, 0, 0xB8, 60, 0, 0, 0, 0x0F, 0x05])
    elif arch in ("aarch64", "arm64"):
        machine = 183
        body = struct.pack("<III", 0xD2800000 | (code << 5), 0xD2800BA8, 0xD4000001)
    else:
        pytest.skip(f"no static probe for machine {arch!r}")
    base, size = 0x400000, 120 + len(body)
    header = b"\x7fELF" + bytes([2, 1, 1, 0]) + bytes(8)
    header += struct.pack(
        "<HHIQQQIHHHHHH", 2, machine, 1, base + 120, 64, 0, 0, 64, 56, 1, 0, 0, 0
    )
    program_header = struct.pack("<IIQQQQQQ", 1, 5, 0, base, base, size, size, 0x1000)
    return header + program_header + body


def _snapshot(root: Path) -> dict[str, tuple[Any, ...]]:
    """Kind, mode, mtime and content (or link target) of every node; two equal snapshots mean no write."""
    nodes: dict[str, tuple[Any, ...]] = {}
    for path in [root, *sorted(root.rglob("*"))]:
        info = path.lstat()
        rel = path.relative_to(root).as_posix()
        if stat.S_ISLNK(info.st_mode):
            nodes[rel] = ("link", os.readlink(path))
        elif stat.S_ISDIR(info.st_mode):
            nodes[rel] = ("dir", info.st_mode, info.st_mtime_ns)
        else:
            nodes[rel] = ("file", info.st_mode, info.st_mtime_ns, path.read_bytes())
    return nodes


# ---------------------------------------------------------------------------
# A relative started path still yields an absolute root
# ---------------------------------------------------------------------------


@posix_only
def test_relative_path_entry_start_survives_tool_chdir(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    launcher = _launcher(out, manifest, "tool")
    relative_dir = os.path.relpath(launcher.parent, tmp_path)
    assert not os.path.isabs(relative_dir)

    through = _run(
        ["tool"],
        cwd=tmp_path,
        env={"PATH": f"{relative_dir}:{CLEAN_PATH}"},
    )

    assert through.returncode == 0, through.stderr
    assert through.stdout.splitlines()[0] == "payload"
    assert f"ran:{out}/" in through.stdout

    # Control: the tool started directly with a relative data path fails after its `cd /`.
    (package,) = manifest["packages"]
    direct = _run(
        [str(out / package["content_dir"] / "bin" / "tool")],
        cwd=tmp_path,
        env={
            "PATH": CLEAN_PATH,
            "DATA_FILE": os.path.relpath(
                out / package["content_dir"] / "share" / "data", tmp_path
            ),
        },
    )
    assert direct.returncode != 0, "a relative root must break after the tool's chdir"


# ---------------------------------------------------------------------------
# A moved directory runs with no `ocx`
# ---------------------------------------------------------------------------


@posix_only
def test_moved_vendored_directory_runs_without_ocx(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    relative = _launcher(out, manifest, "tool").relative_to(out)
    moved = tmp_path / "elsewhere" / "opt" / "tools"
    moved.parent.mkdir(parents=True)
    out.rename(moved)

    result = _run([str(moved / relative)], cwd="/", env={"PATH": CLEAN_PATH})

    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines()[0] == "payload"
    assert f"ran:{moved}/" in result.stdout, "the root is the new location"

    # Control: a record with the old root baked absolute fails after the move.
    record = _record(moved / relative)
    baked = record.read_text(encoding="utf-8").replace(VENDOR_TOKEN, str(out))
    assert str(out) in baked
    record.chmod(0o644)
    record.write_text(baked, encoding="utf-8")
    stale = _run([str(moved / relative)], cwd="/", env={"PATH": CLEAN_PATH})
    assert stale.returncode != 0, "a baked-absolute root cannot survive the move"


# ---------------------------------------------------------------------------
# Lookup is bound to the started image and the record
# ---------------------------------------------------------------------------


def _foreign_tree(tmp_path: Path, out: Path, launcher: Path) -> Path:
    """A copy of the vendored tree with another `vendor_id` whose tool prints `OTHER`."""
    other = tmp_path / "other-tree"
    shutil.copytree(out, other, symlinks=True)
    record = _record(other / launcher.relative_to(out))
    document = json.loads(record.read_text(encoding="utf-8"))
    document["vendor_id"] = "sha256:" + "0" * 64
    program = other / document["program"].removeprefix(f"{VENDOR_TOKEN}/")
    program.write_text("#!/bin/sh\necho OTHER\n", encoding="utf-8")
    program.chmod(0o755)
    record.write_text(json.dumps(document), encoding="utf-8")
    return other


@posix_only
def test_runfiles_lookup_uses_own_tree_not_stale_runfiles_dir(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    launcher = _launcher(out, manifest, "tool")
    other = _foreign_tree(tmp_path, out, launcher)
    started = _stage_runfiles(tmp_path / "bazel-bin" / "pkg", out, launcher, repo="own")
    # A sibling candidate that sorts first but carries another vendor_id.
    shutil.copytree(
        other, started.parent / "tool.runfiles" / "aaa_other", symlinks=True
    )

    result = _run(
        [str(started)],
        cwd=tmp_path,
        env={"PATH": CLEAN_PATH, "RUNFILES_DIR": str(other)},
    )

    assert result.returncode == 0, result.stderr
    assert "OTHER" not in result.stdout
    assert f"ran:{started.parent}/tool.runfiles/own/" in result.stdout

    # Control: the other tree is runnable on its own, so the assertion above could have failed.
    alone = _run(
        [str(other / launcher.relative_to(out))], cwd=tmp_path, env={"PATH": CLEAN_PATH}
    )
    assert alone.stdout.strip() == "OTHER"


@posix_only
def test_runfiles_dir_entry_serves_a_start_path_under_it(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    launcher = _launcher(out, manifest, "tool")
    runfiles = tmp_path / "runfiles"
    runfiles.mkdir()
    shutil.copy2(launcher, runfiles / "tool")
    shutil.copy2(_record(launcher), runfiles / "tool.launch")
    shutil.copytree(out, runfiles / "repo", symlinks=True)
    env = {"PATH": CLEAN_PATH, "RUNFILES_DIR": str(runfiles)}

    result = _run([str(runfiles / "tool")], cwd=tmp_path, env=env)

    assert result.returncode == 0, result.stderr
    assert f"ran:{runfiles}/repo/" in result.stdout

    # Control: the same launcher outside `$RUNFILES_DIR` finds no root.
    outside = tmp_path / "outside"
    outside.mkdir()
    shutil.copy2(launcher, outside / "tool")
    shutil.copy2(_record(launcher), outside / "tool.launch")
    lost = _run([str(outside / "tool")], cwd=tmp_path, env=env)
    assert lost.returncode == EX_CONFIG


@posix_only
def test_fifo_at_candidate_record_does_not_hang(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    launcher = _launcher(out, manifest, "tool")
    started = tmp_path / "bin" / "tool"
    started.parent.mkdir()
    shutil.copy2(launcher, started)
    shutil.copy2(_record(launcher), started.parent / "tool.launch")
    candidate = (
        started.parent
        / "tool.runfiles"
        / "repo"
        / f"{launcher.relative_to(out).as_posix()}.launch"
    )
    candidate.parent.mkdir(parents=True)
    os.mkfifo(candidate)

    result = _run([str(started)], cwd=tmp_path, env={"PATH": CLEAN_PATH}, timeout=5)

    assert result.returncode == EX_CONFIG, result.stderr

    # Control: a plain blocking read of the same FIFO does hang, so the timeout can fire.
    with pytest.raises(subprocess.TimeoutExpired):
        subprocess.run(
            [
                sys.executable,
                "-c",
                "import sys; open(sys.argv[1], 'rb').read()",
                str(candidate),
            ],
            timeout=1,
            check=False,
        )


@posix_only
def test_fifo_as_own_record_exits_78(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    launcher = _launcher(out, manifest, "tool")
    started = tmp_path / "bin" / "tool"
    started.parent.mkdir()
    shutil.copy2(launcher, started)
    os.mkfifo(started.parent / "tool.launch")

    result = _run([str(started)], cwd=tmp_path, env={"PATH": CLEAN_PATH}, timeout=5)

    assert result.returncode == EX_CONFIG, result.stderr


@posix_only
def test_path_hit_that_is_not_the_running_image_exits_78(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    launcher = _launcher(out, manifest, "tool")
    real = _stage_runfiles(tmp_path / "real", out, launcher)
    decoy_dir = tmp_path / "decoy"
    decoy_dir.mkdir()
    shutil.copy2(launcher, decoy_dir / "tool")
    shutil.copy2(_record(launcher), decoy_dir / "tool.launch")

    # Started as `real` but named `tool`: the `PATH` search hits the decoy, a different file.
    mismatched = _run(
        ["tool"],
        cwd=tmp_path,
        env={"PATH": f"{decoy_dir}:{CLEAN_PATH}"},
        executable=real,
    )
    assert mismatched.returncode == EX_CONFIG, mismatched.stderr

    # Control: the same start whose `PATH` hit is the running image runs.
    matched = _run(
        ["tool"],
        cwd=tmp_path,
        env={"PATH": f"{real.parent}:{CLEAN_PATH}"},
        executable=real,
    )
    assert matched.returncode == 0, matched.stderr
    assert matched.stdout.splitlines()[0] == "payload"


# ---------------------------------------------------------------------------
# One tool starts another by bare name
# ---------------------------------------------------------------------------


@posix_only
def test_tool_starts_second_package_tool_through_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    first = _script_package(
        ocx, unique_repo, tmp_path / "first", {"first": "second"}, env=[PATH_ENTRY]
    )
    second = _script_package(
        ocx,
        f"{unique_repo}_second",
        tmp_path / "second",
        {"second": 'echo "second-ran:$0"'},
        env=[PATH_ENTRY],
    )
    out, manifest = _vendor_project(
        ocx, tmp_path / "proj", {"first": first, "second": second}
    )
    first_launcher = _launcher(out, manifest, "first")

    # The selection's composed `PATH` already holds the second package's `bin/`.
    found = _run([str(first_launcher)], cwd=tmp_path, env={"PATH": CLEAN_PATH})
    assert found.returncode == 0, found.stderr
    assert "second-ran:" in found.stdout

    # Control: a set that does not select the second package cannot find the bare name.
    alone_out, alone_manifest = _vendor_project(
        ocx, tmp_path / "proj_alone", {"first": first}
    )
    missing = _run(
        [str(_launcher(alone_out, alone_manifest, "first"))],
        cwd=tmp_path,
        env={"PATH": CLEAN_PATH},
    )
    assert missing.returncode != 0
    assert "second-ran:" not in missing.stdout


# ---------------------------------------------------------------------------
# The launcher writes nothing
# ---------------------------------------------------------------------------


@posix_only
def test_launcher_from_read_only_tree_leaves_tree_unchanged(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    launcher = _launcher(out, manifest, "tool")

    # Control: the snapshot sees a write beside the launcher.
    scratch = tmp_path / "scratch"
    shutil.copytree(out, scratch, symlinks=True)
    scratch_before = _snapshot(scratch)
    (scratch / launcher.relative_to(out).parent / "written").write_text("x")
    assert _snapshot(scratch) != scratch_before

    nodes = [out, *out.rglob("*")]
    for node in nodes:
        if not node.is_symlink():
            node.chmod(node.stat().st_mode & ~0o222)
    try:
        snapshot_before = _snapshot(out)
        result = _run([str(launcher)], cwd=tmp_path, env={"PATH": CLEAN_PATH})
        snapshot_after = _snapshot(out)
    finally:
        for node in nodes:
            if not node.is_symlink():
                node.chmod(node.stat().st_mode | 0o200)

    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines()[0] == "payload"
    assert snapshot_after == snapshot_before


# ---------------------------------------------------------------------------
# No shell on the image
# ---------------------------------------------------------------------------

DISTROLESS = "gcr.io/distroless/static"


def _docker(*args: str, timeout: float = 120) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["docker", *args],
        capture_output=True,
        encoding="utf-8",
        stdin=subprocess.DEVNULL,
        timeout=timeout,
        check=False,
    )


def test_launcher_starts_on_image_without_a_shell(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    if sys.platform != "linux" or not current_platform().startswith("linux/"):
        pytest.skip("a Linux image runs only a Linux launcher")
    try:
        probe = _docker("info", "--format", "{{.ServerVersion}}")
    except FileNotFoundError as err:
        pytest.skip(f"docker is not runnable here: {err}")
    if probe.returncode != 0:
        pytest.skip(
            f"docker info failed: {probe.stdout.strip()} {probe.stderr.strip()}"
        )

    def build(root: Path) -> None:
        (root / "bin").mkdir()
        tool = root / "bin" / "tool"
        tool.write_bytes(_static_exit_elf(7))
        tool.chmod(0o755)

    pkg = publish_tree(
        ocx, unique_repo, "1.0.0", tmp_path / "pkg", build, {"env": [PATH_ENTRY]}
    )
    out, manifest = _vendor_project(ocx, tmp_path / "proj", {"tool": pkg})
    launcher = _launcher(out, manifest, "tool")
    name = f"rules-ocx-rig-p8-{uuid4().hex[:10]}"
    base = ["run", "--rm", "--name", name, "-v", f"{out}:/vendored:ro"]
    try:
        started = _docker(
            *base,
            "--entrypoint",
            f"/vendored/{launcher.relative_to(out).as_posix()}",
            DISTROLESS,
        )
        # Control: the image holds no shell, so a shell-wrapped launcher could not have started.
        shell = _docker(*base, "--entrypoint", "/bin/sh", DISTROLESS, "-c", "exit 0")
    finally:
        _docker("rm", "-f", name)

    assert started.returncode == 7, started.stderr
    assert shell.returncode != 0, "the image must have no /bin/sh"


# ---------------------------------------------------------------------------
# Mach-O signature (macOS only)
# ---------------------------------------------------------------------------


@darwin_only
def test_launcher_survives_copy_hardlink_rename_and_tar_round_trip(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    out, manifest = vendored_tool
    relative = _launcher(out, manifest, "tool").relative_to(out)

    copied = tmp_path / "copied"
    shutil.copytree(out, copied, symlinks=True)
    hardlinked = tmp_path / "hardlinked"
    shutil.copytree(out, hardlinked, symlinks=True, copy_function=os.link)
    archive = tmp_path / "out.tar"
    with tarfile.open(archive, "w") as tar:
        tar.add(out, arcname="tarred")
    with tarfile.open(archive) as tar:
        tar.extractall(tmp_path, filter="tar")
    renamed = tmp_path / "renamed"
    out.rename(renamed)

    for tree in (copied, hardlinked, tmp_path / "tarred", renamed):
        result = _run([str(tree / relative)], cwd=tmp_path, env={"PATH": CLEAN_PATH})
        assert result.returncode == 0, f"{tree.name}: {result.stderr}"


def _codesign_verify(path: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["codesign", "--verify", str(path)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )


@darwin_only
def test_launcher_signature_verifies_intact_and_fails_once_changed_or_removed(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    """The darwin/arm64 launcher's ad-hoc signature is what holds, not a kernel kill.

    The hosted macOS arm64 image starts a changed and an unsigned copy alike, so
    only `codesign --verify` is asserted on them; their runs are printed.
    """
    if platform.machine() != "arm64":
        pytest.skip("only the aarch64-apple-darwin launcher blob carries a signature")
    out, manifest = vendored_tool
    relative = _launcher(out, manifest, "tool").relative_to(out)
    intact = out / relative

    verified = _codesign_verify(intact)
    assert verified.returncode == 0, verified.stderr
    clean = _run([str(intact)], cwd=tmp_path, env={"PATH": CLEAN_PATH})
    assert clean.returncode == 0, clean.stderr

    changed_tree = tmp_path / "changed"
    shutil.copytree(out, changed_tree, symlinks=True)
    changed = changed_tree / relative
    data = bytearray(changed.read_bytes())
    data[len(data) // 4] ^= 0xFF
    changed.write_bytes(data)
    rejected = _codesign_verify(changed)
    assert rejected.returncode != 0, "a one-byte change must fail codesign --verify"

    stripped_tree = tmp_path / "stripped"
    shutil.copytree(out, stripped_tree, symlinks=True)
    stripped = stripped_tree / relative
    subprocess.run(
        ["codesign", "--remove-signature", str(stripped)],
        check=True,
        capture_output=True,
    )
    unsigned = _codesign_verify(stripped)
    assert unsigned.returncode != 0, (
        "a copy without its signature must fail codesign --verify"
    )

    for name, copy in (("changed", changed), ("unsigned", stripped)):
        run = _run([str(copy)], cwd=tmp_path, env={"PATH": CLEAN_PATH})
        print(f"{name} copy exited {run.returncode}: {run.stderr.strip()}")


@darwin_only
def test_stripped_launcher_copy_differs_from_the_signed_blob(
    vendored_tool: tuple[Path, dict[str, Any]], tmp_path: Path
) -> None:
    if platform.machine().lower() not in ("arm64", "aarch64"):
        pytest.skip(
            "the x86_64-apple-darwin launcher blob is unsigned by design; "
            "only the aarch64-apple-darwin blob carries LC_CODE_SIGNATURE"
        )
    out, manifest = vendored_tool
    launcher = _launcher(out, manifest, "tool")
    stripped = tmp_path / "stripped"
    shutil.copy2(launcher, stripped)

    subprocess.run(
        ["codesign", "--remove-signature", str(stripped)],
        check=True,
        capture_output=True,
    )

    assert stripped.read_bytes() != launcher.read_bytes()
    shown = subprocess.run(
        ["codesign", "--verify", str(launcher)], capture_output=True, check=False
    )
    assert shown.returncode == 0, "the vendored launcher keeps its signature"


# ---------------------------------------------------------------------------
# Windows batch targets and case folding
# ---------------------------------------------------------------------------

INJECTION_MARK = "INJECTED_MARK_9f3a"
HOSTILE_ARGS = [
    'a"b',
    "%PATH%",
    f'"&echo {INJECTION_MARK}&"',
    "p|q",
    "sp ace",
    f"x&echo {INJECTION_MARK}",
]


def _injected(stdout: str) -> bool:
    return any(line.startswith(INJECTION_MARK) for line in stdout.splitlines())


@windows_only
def test_cmd_target_with_hostile_arguments_runs_no_injected_command(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    pkg = entrypoint_package(ocx, unique_repo, tmp_path / "pkg")
    out, manifest = _vendor_project(ocx, tmp_path / "proj", {"ep": pkg})
    (package,) = manifest["packages"]
    (gamma,) = [e for e in package["entrypoints"] if e["name"] == "gamma"]
    env = {k: v for k, v in os.environ.items()}

    for argument in HOSTILE_ARGS:
        result = _run(
            [str(out / gamma["launcher_path"]), argument], cwd=tmp_path, env=env
        )
        assert result.returncode in (0, 64), f"{argument!r}: {result.stderr}"
        assert not _injected(result.stdout), f"{argument!r} ran an injected command"

    # Control: the `list2cmdline` quoter lets the same argument inject through the same `.cmd`.
    cmd = out / package["content_dir"] / "bin" / "gamma.cmd"
    quoted = _run([str(cmd), HOSTILE_ARGS[2]], cwd=tmp_path, env=env)
    assert _injected(quoted.stdout), (
        "the control must show the injection it guards against"
    )


@windows_only
def test_environment_variable_names_fold_case_on_windows(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    from src.helpers import make_package_with_entrypoints

    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path / "pkg",
        ["probe"],
        bins=["probe"],
        # On Windows `bins=["probe"]` writes `bin/probe.bat`, and the default PATHEXT order takes `.bat` before `.cmd`,
        # so the probe overwrites that file; a `probe.cmd` beside it would never run.
        extra_files={"bin/probe.bat": "@set\n"},
        env=[PATH_ENTRY],
    )
    out, manifest = _vendor_project(ocx, tmp_path / "proj", {"probe": pkg})
    (package,) = manifest["packages"]
    (probe,) = package["entrypoints"]
    env = {k: v for k, v in os.environ.items() if k.upper() != "PATH"}
    env["Path"] = os.environ.get("PATH", os.environ.get("Path", ""))

    result = _run([str(out / probe["launcher_path"])], cwd=tmp_path, env=env)

    assert result.returncode == 0, result.stderr
    path_lines = [
        line for line in result.stdout.splitlines() if line.upper().startswith("PATH=")
    ]
    assert len(path_lines) == 1, (path_lines, result.stdout)
    assert str(out / package["package_dir"]).replace("/", "\\") in path_lines[0]

    # Control: `set` reports the caller's own spelling, so a second spelling would show.
    plain = _run(["cmd", "/c", "set"], cwd=tmp_path, env=env)
    assert any(line.startswith("Path=") for line in plain.stdout.splitlines())
