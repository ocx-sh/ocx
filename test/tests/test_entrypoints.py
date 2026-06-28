"""Acceptance tests — Entry points happy path + error cases.

Post-flatten layout: per-repo `current` symlink targets the package root, and
`<current>/entrypoints` exposes the generated launchers (no separate
`entrypoints-current` symlink).
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import make_package, make_package_with_entrypoints
from src.registry import fetch_manifest_digest
from src.runner import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# Helpers shared by new suites
# ---------------------------------------------------------------------------


def _dep_entry_ep(ocx: OcxRunner, pkg: PackageInfo, *, visibility: str) -> dict:
    """Build a dependency descriptor with explicit visibility (no default fallback)."""
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    return {"identifier": f"{pkg.fq}@{digest}", "visibility": visibility}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def ocx_home_symlinks(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Return the symlinks/{registry}/{repo}/ directory for the package."""
    from src.runner import registry_dir  # noqa: PLC0415
    reg = registry_dir(ocx.registry)
    return Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg.repo


def current_entrypoints(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Path to the launcher directory reached via the per-repo `current` anchor."""
    return ocx_home_symlinks(ocx, pkg) / "current" / "entrypoints"


# ---------------------------------------------------------------------------
# Happy path: install + select + launcher files reachable via current
# ---------------------------------------------------------------------------


def test_entrypoint_launcher_files_created_after_select(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """install --select wires `current` so launchers are reachable at `current/entrypoints/`."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    entrypoints_dir = current_entrypoints(ocx, pkg)
    assert entrypoints_dir.is_dir(), (
        f"current/entrypoints/ must exist after install --select: {entrypoints_dir}"
    )
    assert (entrypoints_dir / "hello").exists(), (
        f"unix launcher must be reachable at current/entrypoints/hello: {entrypoints_dir / 'hello'}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher test")
def test_entrypoint_unix_launcher_is_executable(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Unix launcher file must exist (via current/entrypoints) and be executable (+x)."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    launcher = current_entrypoints(ocx, pkg) / "hello"
    assert launcher.exists(), f"unix launcher must be generated: {launcher}"
    mode = launcher.stat().st_mode
    assert mode & stat.S_IXUSR, f"unix launcher must be executable: {launcher}, mode={oct(mode)}"


def test_deselect_removes_current_symlink(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx deselect` removes the `current` symlink, severing PATH access to launchers."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)
    ocx.plain("package", "deselect", pkg.short)

    current = ocx_home_symlinks(ocx, pkg) / "current"
    assert not current.exists() and not current.is_symlink(), (
        f"current must be removed after deselect: {current}"
    )


def test_install_without_entrypoints_leaves_current_entrypoints_absent(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """A package without entrypoints must not produce a current/entrypoints dir."""
    pkg = published_package
    ocx.plain("package", "install", "--select", pkg.short)
    entrypoints_dir = current_entrypoints(ocx, pkg)
    assert not entrypoints_dir.exists(), (
        f"current/entrypoints/ must not exist for pkg without entrypoints: {entrypoints_dir}"
    )


# ---------------------------------------------------------------------------
# Error cases
# ---------------------------------------------------------------------------


def test_invalid_entrypoint_name_rejected_at_validation(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Metadata with invalid entrypoint name (uppercase) is rejected at validation time.

    The custom `Entrypoints` deserializer enforces the name regex at deserialize time,
    so `package create` fails as soon as it parses the metadata file — no push needed.
    """
    del unique_repo  # rejection is local; no registry interaction

    pkg_dir = tmp_path / "pkg-invalid-ep"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)
    script = bin_dir / "hello"
    script.write_text("#!/bin/sh\necho hello\n")
    if sys.platform != "win32":
        script.chmod(script.stat().st_mode | stat.S_IEXEC)

    metadata_path = tmp_path / "metadata-invalid-ep.json"
    metadata_obj = {
        "type": "bundle", "version": 1, "entrypoints": {"INVALID_UPPER": {}},
    }
    metadata_path.write_text(json.dumps(metadata_obj))

    bundle = tmp_path / "bundle-invalid-ep.tar.xz"
    result = ocx.run(
        "package", "create", "-m", str(metadata_path), "-o", str(bundle), str(pkg_dir),
        check=False,
    )
    assert result.returncode != 0, (
        "package create with invalid entrypoint name must fail at metadata parse\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "INVALID_UPPER" in result.stderr, (
        f"error must cite the offending name; stderr was: {result.stderr}"
    )


def test_install_without_select_does_not_trigger_collision_check(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Install alone (no --select) must not trigger collision check even with duplicate names."""
    repo_a = f"{unique_repo}-ca"
    repo_b = f"{unique_repo}-cb"

    pkg_a = make_package_with_entrypoints(
        ocx, repo_a, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
    )
    pkg_b = make_package_with_entrypoints(
        ocx, repo_b, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
    )

    ocx.plain("package", "install", "--select", pkg_a.short)
    result = ocx.run("package", "install", pkg_b.short, check=False)
    assert result.returncode == 0, (
        f"install without --select must succeed even when entrypoint names collide; "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )


def test_reselect_to_package_without_entrypoints_drops_entrypoints_dir(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Re-selecting to a package without entrypoints must leave `current/entrypoints` absent.

    The flat layout achieves this via a single `current` flip: pointing it at a
    package root that has no `entrypoints/` child is enough — the launchers
    from the previous package are no longer reachable through `current`.
    """
    pkg_with = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
        tag="1.0.0",
    )
    pkg_without = make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False)

    ocx.plain("package", "install", "--select", pkg_with.short)
    entrypoints_dir = current_entrypoints(ocx, pkg_with)
    assert entrypoints_dir.is_dir(), (
        f"precondition: pkg_with must materialize current/entrypoints/: {entrypoints_dir}"
    )

    ocx.plain("package", "install", "--select", pkg_without.short)
    assert not entrypoints_dir.exists(), (
        f"re-selecting to a package without entrypoints must leave current/entrypoints/ unreachable; "
        f"still present at {entrypoints_dir}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher invocation test")
def test_launcher_invocation_runs_target_and_forwards_args(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Invoking the generated launcher with extra args runs the resolved target and forwards args."""
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
        tag="1.0.0",
    )
    ocx.plain("package", "install", "--select", pkg.short)

    launcher = current_entrypoints(ocx, pkg) / "hello"
    assert launcher.exists(), f"unix launcher must exist: {launcher}"

    extra = "extra-arg-from-test"
    launcher_env = dict(ocx.env)
    launcher_env["PATH"] = f"{ocx.binary.parent}{os.pathsep}{launcher_env.get('PATH', '')}"
    completed = subprocess.run(
        [str(launcher), extra],
        capture_output=True,
        text=True,
        env=launcher_env,
        timeout=30,
        check=False,
    )
    assert completed.returncode == 0, (
        f"launcher invocation must succeed; rc={completed.returncode} "
        f"stderr={completed.stderr.strip()!r}"
    )
    assert pkg.marker in completed.stdout, (
        f"launcher must invoke the resolved target — package marker missing; "
        f"stdout={completed.stdout!r}"
    )
    assert extra in completed.stdout, (
        f"launcher must forward CLI args verbatim; stdout={completed.stdout!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher invocation test")
def test_baked_args_resolved_and_prepended_before_user_args(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Baked `args` with `${installPath}` are resolved and prepended before user args.

    Contract 5 (prepend order) + Contract 6 (`${installPath}` → content path):
    an entrypoint with ``args: ["${installPath}/data/ref.txt"]`` and launcher
    invocation ``launcher USERARG`` must dispatch the binary with argv:
    ``<content>/data/ref.txt  USERARG``  (baked first, user appended, in order).

    The shipped file ``data/ref.txt`` is bundled inside the package; the test
    asserts the resolved path is an absolute path whose directory ancestry
    includes ``content/`` — the `${installPath}` anchor defined by the runtime
    as ``validated.join("content")``.

    Verifies baked args are resolved and prepended — Phase 3 runtime implemented
    in ``crates/ocx_cli/src/command/launcher/exec.rs``.
    """
    shipped_rel = "data/ref.txt"
    user_arg = "USERARG"

    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"hello": {"args": [f"${{installPath}}/{shipped_rel}"]}},
        bins=["hello"],
        tag="1.0.0",
        extra_files={shipped_rel: "baked-arg-target-file"},
    )
    ocx.plain("package", "install", "--select", pkg.short)

    launcher = current_entrypoints(ocx, pkg) / "hello"
    assert launcher.exists(), f"unix launcher must exist: {launcher}"

    launcher_env = dict(ocx.env)
    launcher_env["PATH"] = f"{ocx.binary.parent}{os.pathsep}{launcher_env.get('PATH', '')}"
    completed = subprocess.run(
        [str(launcher), user_arg],
        capture_output=True,
        text=True,
        env=launcher_env,
        timeout=30,
        check=False,
    )
    assert completed.returncode == 0, (
        f"launcher invocation must succeed; rc={completed.returncode} "
        f"stderr={completed.stderr.strip()!r}"
    )

    stdout = completed.stdout

    # Basic sanity: the dispatched binary ran (marker present in output).
    assert pkg.marker in stdout, (
        f"package marker missing — dispatched binary did not run; stdout={stdout!r}"
    )

    # Contract 6: ${installPath} resolves to an absolute path in content/.
    # Without the Phase 3 runtime implementation the baked arg is not forwarded
    # to the binary, so shipped_rel will be absent — the assert below fails.
    baked_pos = stdout.find(shipped_rel)
    assert baked_pos != -1, (
        f"resolved baked arg suffix '{shipped_rel}' not found in stdout; "
        f"baked args were not prepended or ${'{installPath}'} was not resolved "
        f"(runtime stub in launcher/exec.rs not yet implemented); "
        f"stdout={stdout!r}"
    )

    # Extract the full resolved token to verify it is an absolute content/ path.
    baked_token = next((t for t in stdout.split() if shipped_rel in t), None)
    assert baked_token is not None  # guaranteed: baked_pos != -1 above
    assert os.path.isabs(baked_token), (
        f"resolved baked arg must be an absolute path; "
        f"got {baked_token!r}; stdout={stdout!r}"
    )
    assert "/content/" in baked_token or baked_token.endswith("/content"), (
        f"resolved baked arg must be inside the installed package content/ directory "
        f"(${{installPath}} = validated.join('content')); "
        f"got {baked_token!r}; stdout={stdout!r}"
    )

    # Contract 5: baked arg is prepended BEFORE user args (strict left-to-right order).
    user_pos = stdout.find(user_arg)
    assert user_pos != -1, (
        f"user arg '{user_arg}' not found in stdout; stdout={stdout!r}"
    )
    assert baked_pos < user_pos, (
        f"baked arg (at char {baked_pos}) must appear BEFORE user arg "
        f"(at char {user_pos}) in dispatched argv; stdout={stdout!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher invocation test")
def test_launcher_dispatches_divergent_command(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """An entrypoint whose ``command`` differs from its name dispatches the command.

    The package exposes the invocable name ``hello`` but declares
    ``command: hello-bin``; only ``hello-bin`` exists on the composed PATH
    (there is no ``hello`` binary). The launcher named ``hello`` must run
    ``hello-bin`` — proving the name -> command mapping in ``ocx launcher
    exec`` rather than a coincidental name-on-PATH resolution.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path,
        entrypoints={"hello": {"command": "hello-bin"}},
        bins=["hello-bin"],
        tag="1.0.0",
    )
    ocx.plain("package", "install", "--select", pkg.short)

    launcher = current_entrypoints(ocx, pkg) / "hello"
    assert launcher.exists(), f"launcher must be named after the invocable name: {launcher}"
    assert not (launcher.parent / "hello-bin").exists(), (
        "no launcher is generated for the dispatch command — only the invocable name"
    )

    launcher_env = dict(ocx.env)
    launcher_env["PATH"] = f"{ocx.binary.parent}{os.pathsep}{launcher_env.get('PATH', '')}"
    completed = subprocess.run(
        [str(launcher)],
        capture_output=True,
        text=True,
        env=launcher_env,
        timeout=30,
        check=False,
    )
    assert completed.returncode == 0, (
        f"divergent-command launcher must succeed; rc={completed.returncode} "
        f"stderr={completed.stderr.strip()!r}"
    )
    assert f"entry-point-hello-bin {pkg.marker}" in completed.stdout, (
        f"launcher 'hello' must dispatch 'hello-bin'; stdout={completed.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Synthetic PATH entry: entrypoints/ added to env
# ---------------------------------------------------------------------------


def test_root_package_entrypoints_appear_in_self_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A root package with entrypoints must have its own entrypoints/ dir in PATH via ocx env.

    The visible-package pipeline emits a synthetic `PATH ⊳ <pkg_root>/entrypoints`
    entry so that the installed launchers are reachable after `eval $(ocx shell env ...)`.
    """
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)
    path_entries = [e["value"] for e in env_result["entries"] if e["key"] == "PATH"]

    # At least one PATH entry must contain the entrypoints/ subdirectory.
    assert any("entrypoints" in v for v in path_entries), (
        f"expected an entrypoints/ PATH entry in env output; PATH values: {path_entries}"
    )


def test_synthetic_entrypoints_path_emitted_after_declared_bin(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The synthetic `entrypoints/` PATH entry must be emitted AFTER the
    declared `${installPath}/bin` PATH entry in `ocx env` output.

    `ocx env` lists PATH-typed entries in apply order. Consumers process them
    by prepending, so the LAST entry in the list ends up FIRST in the resolved
    PATH. Putting the synthetic `entrypoints/` entry after the declared `bin/`
    entry in the output therefore makes `entrypoints/` win lookup priority —
    entrypoint launchers shadow declared `bin/` so the canonical `ocx launcher
    exec` re-entry is the one PATH lookup finds first. Required global emit
    order: ``Deps > Env > Entrypoints``.

    The fixture's declared ``bin/`` PATH entry is marked ``visibility: public``
    so it surfaces under the default ``--mode=consumer`` alongside the
    synthetic ``entrypoints/`` entry (which is interface-tagged and therefore
    consumer-visible). Both appear and their ordering can be verified.

    Acceptance-level mirror of the unit test in
    `crates/ocx_lib/src/package_manager/composer.rs::emit_root_path_block_declared_bin_precedes_synth_path_consumer_surface`.
    """
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            },
        ],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)
    path_entries = [(i, e["value"]) for i, e in enumerate(env_result["entries"]) if e["key"] == "PATH"]
    assert path_entries, f"expected PATH entries in env output: {env_result}"

    # On Windows the bin segment uses backslashes; match either separator.
    # Anchor on the path tail — the pytest tmp_path dir name contains
    # "entrypoints" because the test name does, so a loose substring check
    # would also match the bin/ entry.
    syn_idx = next(
        (i for i, v in path_entries if v.endswith("/entrypoints") or v.endswith("\\entrypoints")),
        None,
    )
    bin_idx = next(
        (i for i, v in path_entries if v.endswith("/bin") or v.endswith("\\bin")),
        None,
    )

    assert syn_idx is not None, (
        f"synthetic entrypoints PATH entry missing; PATH values: {[v for _, v in path_entries]}"
    )
    assert bin_idx is not None, (
        f"declared bin/ PATH entry missing; PATH values: {[v for _, v in path_entries]}"
    )
    assert bin_idx < syn_idx, (
        f"declared bin/ entry (index {bin_idx}) must precede synthetic entrypoints entry "
        f"(index {syn_idx}) in env output; values: {[v for _, v in path_entries]}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix exec integration test")
def test_exec_dep_launcher_via_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """exec A -- cmake executes B's cmake binary when A declares B as a public dep.

    B's cmake entrypoint must be reachable through the synthetic PATH entry that
    the visible-package pipeline emits for B's entrypoints/ directory. The
    synth-PATH entry is added LAST in the env list (and so ends up FIRST in the
    resolved PATH), so exec finds B's launcher first; the launcher re-enters via
    `ocx launcher exec` and execs B's `bin/cmake` by absolute path — the real
    binary runs and the marker appears in stdout.
    """
    b_repo = f"{unique_repo}_b"
    a_repo = f"{unique_repo}_a"

    pkg_b = make_package_with_entrypoints(
        ocx,
        b_repo,
        tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="b",
    )

    dep_digest = fetch_manifest_digest(ocx.registry, b_repo, "1.0.0")
    dep_entry = {
        "identifier": f"{pkg_b.fq}@{dep_digest}",
        "visibility": "public",
    }

    from src.helpers import make_package  # noqa: PLC0415
    pkg_a = make_package(
        ocx,
        a_repo,
        "1.0.0",
        tmp_path,
        dependencies=[dep_entry],
    )

    ocx.plain("package", "install", "--select", pkg_a.short)

    result = ocx.plain("package", "exec", pkg_a.short, "--", "cmake")
    assert result.returncode == 0, (
        f"exec dep launcher must succeed; rc={result.returncode} stderr={result.stderr.strip()!r}"
    )
    assert pkg_b.marker in result.stdout, (
        f"exec must run B's cmake binary — marker missing; stdout={result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Windows: native `.exe` shim resolves via the default PATHEXT (no `.cmd`)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "win32", reason="Windows launcher test")
def test_exec_resolves_native_exe_shim_on_windows(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """ocx exec resolves the native `<name>.exe` shim — no PATHEXT inject.

    Post-cutover (`adr_windows_exe_shim.md` Axis C → C2) the Windows launcher
    is `<name>.exe` + `<name>.shim` only; no `.cmd` is emitted and OCX no
    longer injects `.CMD` into the child PATHEXT. `.EXE` is unconditionally in
    the default Windows PATHEXT, so even an env whose PATHEXT lacks `.CMD`
    entirely resolves the shim and runs the entrypoint.
    """
    import copy

    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    # PATHEXT without .CMD — irrelevant now: the launcher is a `.exe`, and
    # `.EXE` is always in the default Windows PATHEXT.
    stripped_env = copy.copy(ocx.env)
    stripped_env["PATHEXT"] = ".EXE;.BAT;.COM"

    cmd = [str(ocx.binary), "package", "exec", pkg.short, "--", "hello"]
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env=stripped_env,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, (
        "ocx exec must resolve the native `.exe` shim with no PATHEXT inject "
        f"(rc={result.returncode}, stderr={result.stderr.strip()!r})"
    )
    assert pkg.marker in result.stdout, (
        f"exec must run the entrypoint via the `.exe` shim; stdout={result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Closure-scoped collision detection
# ---------------------------------------------------------------------------


def test_env_two_roots_with_same_entrypoint_name_errors(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx env A B` must surface EntrypointCollision with exit code 65 when
    two independent roots declare the same entrypoint name.

    Both packages install without `--select`, so the install-gate
    (`composer::check_entrypoint_collision`, scoped to a single root's
    interface closure) does not fire — the collision is only visible at
    consumption time when the caller asks for the merged env of both roots.
    The compose-time multi-root gate (`composer::check_multi_root_entrypoint_collision`,
    invoked from `composer::compose` whenever two or more roots are passed)
    catches it before any env entries are emitted.
    """
    repo_a = f"{unique_repo}-ea"
    repo_b = f"{unique_repo}-eb"

    pkg_a = make_package_with_entrypoints(
        ocx, repo_a, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
    )
    pkg_b = make_package_with_entrypoints(
        ocx, repo_b, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
    )

    ocx.plain("package", "install", pkg_a.short)
    ocx.plain("package", "install", pkg_b.short)

    result = ocx.run("package", "env", pkg_a.short, pkg_b.short, check=False)
    assert result.returncode == 65, (
        f"ocx env across colliding roots must exit 65 (DataError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert "cmake" in result.stderr, (
        f"error must cite the colliding entrypoint name 'cmake'; "
        f"stderr={result.stderr.strip()!r}"
    )


def test_exec_two_roots_with_same_entrypoint_name_errors(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx exec A B -- <tool>` must surface EntrypointCollision with exit code 65 when
    two independent roots declare the same entrypoint name.

    Mirrors `test_env_two_roots_with_same_entrypoint_name_errors` but exercises
    the `exec` path.  Both packages are installed without `--select` so the
    install-gate does not fire; the multi-root compose-time gate catches the
    collision before any command is executed.
    """
    repo_a = f"{unique_repo}-xa"
    repo_b = f"{unique_repo}-xb"

    pkg_a = make_package_with_entrypoints(
        ocx, repo_a, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
    )
    pkg_b = make_package_with_entrypoints(
        ocx, repo_b, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
    )

    ocx.plain("package", "install", pkg_a.short)
    ocx.plain("package", "install", pkg_b.short)

    result = ocx.run("package", "exec", pkg_a.short, pkg_b.short, "--", "cmake", check=False)
    assert result.returncode == 65, (
        f"ocx exec across colliding roots must exit 65 (DataError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert "cmake" in result.stderr, (
        f"error must cite the colliding entrypoint name 'cmake'; "
        f"stderr={result.stderr.strip()!r}"
    )


def test_install_intra_closure_collision_aborts_before_candidate_symlink(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Stage 1 intra-closure collision must abort before any disk state is created.

    Setup: pkg_a declares `cmake` entrypoint, pkg_b imports pkg_a as a public dep
    AND declares its own `cmake` entrypoint. Pulling pkg_b drags pkg_a into pkg_b's
    visible closure, so the Stage 1 check (`pull.rs::setup_owned`) sees both
    declarations and aborts. The aborted install must leave no candidate symlink
    behind for pkg_b — the collision is detected before the temp→final atomic move.
    """
    from src.registry import fetch_manifest_digest  # noqa: PLC0415
    from src.runner import registry_dir  # noqa: PLC0415

    repo_a = f"{unique_repo}-sa"
    repo_b = f"{unique_repo}-sb"

    pkg_a = make_package_with_entrypoints(
        ocx, repo_a, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="a",
    )
    dep_digest = fetch_manifest_digest(ocx.registry, repo_a, "1.0.0")
    dep_entry = {
        "identifier": f"{pkg_a.fq}@{dep_digest}",
        "visibility": "public",
    }

    pkg_b = make_package_with_entrypoints(
        ocx, repo_b, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="b",
        dependencies=[dep_entry],
    )

    result = ocx.run("package", "install", "--select", pkg_b.short, check=False)
    assert result.returncode == 65, (
        f"install --select with intra-closure collision must exit 65 (DataError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert "cmake" in result.stderr, (
        f"error must cite the colliding entrypoint name 'cmake'; "
        f"stderr={result.stderr.strip()!r}"
    )

    reg = registry_dir(ocx.registry)
    candidate_b = (
        Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg_b.repo / "candidates" / "1.0.0"
    )
    assert not candidate_b.exists() and not candidate_b.is_symlink(), (
        f"pkg_b candidate symlink must not be created after Stage 1 collision; "
        f"found at {candidate_b}"
    )


def test_install_transitive_closure_collision_aborts_before_disk(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Stage 1 transitive-closure collision must abort install and leave no candidate symlink.

    Setup: root depends on A and B (both public). A depends on C (public) which
    declares entrypoint `cmake`. B depends on D (public) which also declares
    entrypoint `cmake`. Neither A nor B declares the entrypoint — the collision
    is only visible when the full transitive closure of root is checked.

    The Stage 1 check in pull.rs uses `import_visible_packages` which walks the
    complete reachable set from root. It must detect the duplicate `cmake` name
    across C and D and abort before the temp→final atomic move — i.e. before the
    root candidate symlink is written to disk.
    """
    from src.registry import fetch_manifest_digest  # noqa: PLC0415
    from src.runner import registry_dir  # noqa: PLC0415

    repo_c = f"{unique_repo}-tc"
    repo_d = f"{unique_repo}-td"
    repo_a = f"{unique_repo}-ta"
    repo_b = f"{unique_repo}-tb"
    repo_root = f"{unique_repo}-tr"

    # C — leaf with `cmake` entrypoint.
    pkg_c = make_package_with_entrypoints(
        ocx, repo_c, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="c",
    )
    c_digest = fetch_manifest_digest(ocx.registry, repo_c, "1.0.0")

    # D — leaf with conflicting `cmake` entrypoint.
    pkg_d = make_package_with_entrypoints(
        ocx, repo_d, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="d",
    )
    d_digest = fetch_manifest_digest(ocx.registry, repo_d, "1.0.0")

    # A — intermediate, no entrypoint, depends publicly on C.
    c_dep_entry = {
        "identifier": f"{pkg_c.fq}@{c_digest}",
        "visibility": "public",
    }
    pkg_a = make_package(
        ocx, repo_a, "1.0.0", tmp_path,
        dependencies=[c_dep_entry],
    )
    a_digest = fetch_manifest_digest(ocx.registry, repo_a, "1.0.0")

    # B — intermediate, no entrypoint, depends publicly on D.
    d_dep_entry = {
        "identifier": f"{pkg_d.fq}@{d_digest}",
        "visibility": "public",
    }
    pkg_b = make_package(
        ocx, repo_b, "1.0.0", tmp_path,
        dependencies=[d_dep_entry],
    )
    b_digest = fetch_manifest_digest(ocx.registry, repo_b, "1.0.0")

    # root — depends publicly on A and B; no entrypoint of its own.
    a_dep_entry = {
        "identifier": f"{pkg_a.fq}@{a_digest}",
        "visibility": "public",
    }
    b_dep_entry = {
        "identifier": f"{pkg_b.fq}@{b_digest}",
        "visibility": "public",
    }
    pkg_root = make_package(
        ocx, repo_root, "1.0.0", tmp_path,
        dependencies=[a_dep_entry, b_dep_entry],
    )

    result = ocx.run("package", "install", "--select", pkg_root.short, check=False)
    assert result.returncode == 65, (
        f"install --select with transitive collision must exit 65 (DataError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert "cmake" in result.stderr, (
        f"error must cite the colliding entrypoint name 'cmake'; "
        f"stderr={result.stderr.strip()!r}"
    )
    assert repo_c in result.stderr and repo_d in result.stderr, (
        f"error must cite both colliding repositories ({repo_c!r}, {repo_d!r}) "
        f"so the user can identify which transitively-reached packages own the "
        f"duplicate entrypoint; stderr={result.stderr.strip()!r}"
    )

    reg = registry_dir(ocx.registry)
    candidate_root = (
        Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg_root.repo / "candidates" / "1.0.0"
    )
    assert not candidate_root.exists() and not candidate_root.is_symlink(), (
        f"root candidate symlink must not be created after transitive Stage 1 collision; "
        f"found at {candidate_root}"
    )


# ---------------------------------------------------------------------------
# Suite A — Entrypoint collision gated on interface projection
# (Step 3.5 of plan_two_env_composition.md)
#
# R and B both declare entrypoint ``e``.  The collision check runs on the
# *interface projection* of R's TC only.  Four edge-visibility cells:
#
#   sealed    → install OK  (B.has_interface()=false → not in R's interface projection)
#   private   → install OK  (B.has_interface()=false → not in R's interface projection)
#   interface → install FAIL (B.has_interface()=true → collision in interface surface)
#   public    → install FAIL (B.has_interface()=true → collision in interface surface)
#
# Each cell is one parameter row.  The test is written so it FAILS once
# Phase 4 replaces the ``unimplemented!()`` stub in
# ``composer::check_entrypoint_collision``.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "edge_visibility,expect_collision",
    [
        pytest.param("sealed", False, id="sealed-no-collision"),
        pytest.param("private", False, id="private-no-collision"),
        pytest.param("interface", True, id="interface-collision"),
        pytest.param("public", True, id="public-collision"),
    ],
)
def test_suite_a_entrypoint_collision_gated_on_interface_projection(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    edge_visibility: str,
    expect_collision: bool,
) -> None:
    """Suite A: install R with dep B (same entrypoint name) at varying edge visibility.

    Collision detection runs only on R's interface projection (plan §3.5).
    ``sealed`` and ``private`` edges produce ``has_interface()=false``; the dep
    does NOT enter the collision set.  ``interface`` and ``public`` edges produce
    ``has_interface()=true``; the duplicate name is caught before any disk write.
    """
    from src.runner import registry_dir  # noqa: PLC0415

    repo_b = f"{unique_repo}_b"

    # B: leaf package with entrypoint ``tool``.
    pkg_b = make_package_with_entrypoints(
        ocx, repo_b, tmp_path,
        entrypoints=["tool"],
        bins=["tool"],
        tag="1.0.0",
        file_prefix="b",
    )

    # R: root package with the same entrypoint ``tool`` + dep B at given vis.
    dep_b = _dep_entry_ep(ocx, pkg_b, visibility=edge_visibility)
    pkg_r = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path,
        entrypoints=["tool"],
        bins=["tool"],
        tag="1.0.0",
        file_prefix="r",
        dependencies=[dep_b],
    )

    result = ocx.run("package", "install", "--select", pkg_r.short, check=False)

    if expect_collision:
        assert result.returncode == 65, (
            f"edge_vis={edge_visibility!r}: expected exit 65 (DataError/EntrypointCollision); "
            f"got rc={result.returncode}, stderr={result.stderr.strip()!r}"
        )
        assert "tool" in result.stderr, (
            f"edge_vis={edge_visibility!r}: error must cite colliding name 'tool'; "
            f"stderr={result.stderr.strip()!r}"
        )
        # Verify no candidate symlink was left behind.
        reg = registry_dir(ocx.registry)
        candidate = (
            Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg_r.repo / "candidates" / "1.0.0"
        )
        assert not candidate.exists() and not candidate.is_symlink(), (
            f"edge_vis={edge_visibility!r}: candidate symlink must not exist after collision; "
            f"found at {candidate}"
        )
    else:
        assert result.returncode == 0, (
            f"edge_vis={edge_visibility!r}: expected install to succeed (no interface collision); "
            f"got rc={result.returncode}, stderr={result.stderr.strip()!r}"
        )


# ---------------------------------------------------------------------------
# Suite E — Closure-deep mixed-edge collision
# (Step 3.9 of plan_two_env_composition.md)
#
# New test: same transitive shape as test_install_transitive_closure_collision_aborts_before_disk
# (R → A,B → C,D, all public, C+D declare ``cmake``) but with A→C edge
# set to ``private``.  C's effective visibility from R is:
#   R→A: public, A→C: private → through_edge: SEALED (private.has_interface()=false)
# C is therefore NOT in R's interface projection → no collision at install time.
# Only D (via R→B→D, all public) reaches R's interface surface.
#
# The test asserts install succeeds and ``ocx exec R`` runs one cmake binary
# (D's, the only one in the interface projection).  It documents the deliberate
# looseness on the private surface — runtime PATH order handles private-surface
# duplicates.
# ---------------------------------------------------------------------------


def test_suite_e_mixed_edge_private_seals_c_from_interface_no_collision(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Suite E: R → A(public) → C(private) and R → B(public) → D(public).

    C and D both declare entrypoint ``cmake``.  The A→C edge is private, so
    C's effective visibility from R is SEALED (plan §3.9: through_edge returns
    SEALED when child does not have_interface).  Only D reaches R's interface
    projection → no collision at install time.

    Phase 4 implements ``check_entrypoint_collision``; until then this test
    passes (install runs the Phase 3 stub which does not abort on collision).
    The test documents the deliberate looseness on the private surface.
    """
    repo_c = f"{unique_repo}_ec"
    repo_d = f"{unique_repo}_ed"
    repo_a = f"{unique_repo}_ea"
    repo_b = f"{unique_repo}_eb"

    # C — leaf with ``cmake`` entrypoint.
    pkg_c = make_package_with_entrypoints(
        ocx, repo_c, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="ec",
    )
    c_digest = fetch_manifest_digest(ocx.registry, repo_c, "1.0.0")

    # D — leaf with conflicting ``cmake`` entrypoint.
    pkg_d = make_package_with_entrypoints(
        ocx, repo_d, tmp_path,
        entrypoints=["cmake"],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="ed",
    )
    d_digest = fetch_manifest_digest(ocx.registry, repo_d, "1.0.0")

    # A — depends on C with PRIVATE edge (C sealed from R's interface projection).
    c_dep_private = {"identifier": f"{pkg_c.fq}@{c_digest}", "visibility": "private"}
    pkg_a = make_package(ocx, repo_a, "1.0.0", tmp_path, dependencies=[c_dep_private])
    a_digest = fetch_manifest_digest(ocx.registry, repo_a, "1.0.0")

    # B — depends on D with PUBLIC edge (D visible in R's interface projection).
    d_dep_public = {"identifier": f"{pkg_d.fq}@{d_digest}", "visibility": "public"}
    pkg_b = make_package(ocx, repo_b, "1.0.0", tmp_path, dependencies=[d_dep_public])
    b_digest = fetch_manifest_digest(ocx.registry, repo_b, "1.0.0")

    # Root — depends on A and B both publicly.
    a_dep = {"identifier": f"{pkg_a.fq}@{a_digest}", "visibility": "public"}
    b_dep = {"identifier": f"{pkg_b.fq}@{b_digest}", "visibility": "public"}
    pkg_root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        dependencies=[a_dep, b_dep],
    )

    # Install must succeed: C is sealed from interface projection, only D is visible.
    result = ocx.run("package", "install", "--select", pkg_root.short, check=False)
    assert result.returncode == 0, (
        "Mixed-edge suite E: A→C(private) seals C from interface projection; "
        "install must succeed (no collision in interface surface); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
