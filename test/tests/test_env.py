"""Tests for OCI-tier per-package env (``ocx package env``).

Rewritten Phase 5 (plan_toolchain_cli.md):
- ``ocx env <pkg>`` → ``ocx package env <pkg>`` (OCI-tier, C3 contract).
- ``ocx shell env <pkg>`` (deleted) → rewritten to assert exit 64.

Note (W5): ``ocx package env`` auto-installs missing packages via
``find_or_install_all`` (deliberate, handshake §2 accepted this cut).
Do NOT assert old 'shell env no-download' semantics against ``package env``.
"""
from __future__ import annotations

import io
import json
import subprocess
import tarfile
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from src import OcxRunner, PackageInfo, registry_dir
from src.helpers import make_package, resolved_metadata_path
from src.registry import fetch_platform_manifest_digest, push_raw_package
from src.runner import current_platform

# Exit code for deleted commands
EXIT_USAGE = 64
# DataError (sysexits EX_DATAERR) — every interpolation refusal classifies here.
EXIT_DATA_ERR = 65


def test_env_path_contains_bin(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env <pkg> → PATH includes /bin"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)
    path_entry = next(e for e in env_result["entries"] if e["key"] == "PATH")
    assert "/bin" in path_entry["value"] or "\\bin" in path_entry["value"]


def test_env_constant_contains_content_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env <pkg> — constant var points to content dir"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("package", "env", pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert registry_dir(ocx.registry) in home_entry["value"]
    # CAS layout: packages/{registry}/sha256/{prefix}/{suffix}/content
    assert "packages" in home_entry["value"]


def test_env_candidate_uses_symlink_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env --candidate <pkg>"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("package", "env", "--candidate", pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert f"candidates/{pkg.tag}" in home_entry["value"] or f"candidates\\{pkg.tag}" in home_entry["value"]


def test_shell_env_removed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """``ocx shell env <pkg>`` → exit 64 (deleted command, plan C4).

    The ``ocx shell env`` command is removed. Per-package env is now
    ``ocx package env``; sourceable form uses ``--shell[=NAME]``.
    """
    pkg = published_package
    result = subprocess.run(
        [str(ocx.binary), "shell", "env", pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell env must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# adr_declared_binaries_metadata.md §4 — `binaries`/`entrypoints` JSON arrays
# ---------------------------------------------------------------------------


def test_package_env_json_binaries_array_has_package_attribution(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx package env`'s JSON `binaries` array names the admitted package
    that declared each claim (`package` = the admitted `PinnedIdentifier`'s
    string form, ADR §4 Decision A)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)

    digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    assert env_result["binaries"] == [{"name": "hello", "package": f"{pkg.fq}@{digest}"}], (
        env_result["binaries"]
    )
    assert env_result["entrypoints"] == [], env_result["entrypoints"]


def test_package_env_json_binaries_entrypoints_present_but_empty_without_claims(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`binaries`/`entrypoints` are always present as arrays — possibly
    empty — even for a package that declares neither (ADR §4).

    `--no-bin-scan` keeps the claim genuinely absent despite an
    interface-visible executable in `bin/` — Auto mode (the default) would
    otherwise fill it, which is exactly the behavior under test elsewhere.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], no_bin_scan=True)
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)

    assert env_result["binaries"] == []
    assert env_result["entrypoints"] == []


def test_package_env_shell_output_excludes_binaries_and_entrypoints(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--shell` stays the eval-safe channel only — a declared `binaries`
    claim never leaks into it (ADR §4: both sinks return before `EnvVars`
    is even constructed)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short, "--shell=bash")

    assert result.returncode == 0, result.stderr
    assert "export" in result.stdout, result.stdout
    assert "binaries" not in result.stdout.lower(), result.stdout
    assert "hello" not in result.stdout, result.stdout


def test_package_env_ci_github_output_excludes_binaries_and_entrypoints(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--ci=github`` stays a CI persistence sink only — a declared `binaries`
    claim never leaks into the ``$GITHUB_ENV`` / ``$GITHUB_PATH`` sink files
    (ADR §4: both sinks return before `EnvVars` is even constructed)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    github_path = tmp_path / "github_path"
    github_env = tmp_path / "github_env"
    github_path.write_text("")
    github_env.write_text("")

    result = ocx.plain(
        "package",
        "env",
        pkg.short,
        "--ci=github",
        env_overrides={
            "GITHUB_ACTIONS": "true",
            "GITHUB_PATH": str(github_path),
            "GITHUB_ENV": str(github_env),
        },
    )

    assert result.returncode == 0, result.stderr
    sink_text = github_path.read_text() + github_env.read_text()
    assert "binaries" not in sink_text.lower(), sink_text
    assert "hello" not in sink_text, sink_text


def test_package_env_plain_shows_hint_when_binaries_present(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Decision C: the plain `entries` table stays byte-stable; a hint line
    below it announces binary/entrypoint availability when the admitted set
    carries any claims."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short)

    assert result.returncode == 0, result.stderr
    assert "hello" in result.stdout, result.stdout
    assert "available" in result.stdout.lower(), result.stdout


def test_package_env_plain_omits_hint_without_claims(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """No hint line renders when the admitted set carries no binaries or
    entrypoints (the common case, unaffected by this feature).

    `--no-bin-scan` keeps the claim genuinely absent despite an
    interface-visible executable in `bin/`.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], no_bin_scan=True)
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short)

    assert result.returncode == 0, result.stderr
    assert "available" not in result.stdout.lower(), result.stdout


# ---------------------------------------------------------------------------
# `--env` on the OCI tier
#
# `--env` is a per-invocation CLI override, not project configuration, so it
# lands here too. It does NOT make this tier read `ocx.toml` — there is no
# `-g`, no project `[env]`, nothing but what the caller typed.
# ---------------------------------------------------------------------------


def _export_value(lines: str, key: str) -> str | None:
    """Return the effective value of ``export KEY="value"`` in a shell block.

    Returns the LAST assignment: the block replays the composed entry vector,
    so a later-stage constant is emitted as a second ``export`` line and a
    shell evaluating top to bottom keeps the last one.
    """
    prefix = f"export {key}="
    effective: str | None = None
    for line in lines.splitlines():
        if not line.startswith(prefix):
            continue
        value = line[len(prefix) :]
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        effective = value
    return effective


def _dumped_value(dump: str, key: str) -> str | None:
    """Return the value of a ``KEY=value`` line in an ``env``-dumped block."""
    prefix = f"{key}="
    for line in dump.splitlines():
        if line.startswith(prefix):
            return line[len(prefix) :]
    return None


def test_package_env_flag_matches_what_package_exec_executes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx package env --env`` prints what ``ocx package exec --env`` runs with.

    The export/execute parity that justifies the flag existing on an emit-only
    command, asserted against one shared oracle so a divergence between the two
    fails rather than two independently-passing assertions.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])
    ocx.plain("package", "install", pkg.short)

    overrides = ["--env", "FROM_FLAG=flag-value"]

    executed = subprocess.run(
        [str(ocx.binary), "package", "exec", *overrides, pkg.short, "--", "env"],
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert executed.returncode == 0, executed.stderr

    exported = subprocess.run(
        [str(ocx.binary), "package", "env", *overrides, "--shell=bash", pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert exported.returncode == 0, exported.stderr

    assert _dumped_value(executed.stdout, "FROM_FLAG") == "flag-value", (
        f"package exec --env must apply the override; stdout:\n{executed.stdout}"
    )
    assert _export_value(exported.stdout, "FROM_FLAG") == "flag-value", (
        f"package env --env must export the same value package exec applies; "
        f"export lines:\n{exported.stdout}"
    )


def test_package_env_flag_does_not_read_ocx_toml(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The OCI tier stays OCI-tier: an `ocx.toml` beside the invocation
    contributes nothing, even though `--env` now composes here.

    Pins the boundary the flag deliberately does not cross.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])
    ocx.plain("package", "install", pkg.short)

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text('[tools]\n\n[env]\nFROM_FILE = "must-not-appear"\n')

    result = subprocess.run(
        [str(ocx.binary), "package", "exec", "--env", "FROM_FLAG=ok", pkg.short, "--", "env"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert _dumped_value(result.stdout, "FROM_FLAG") == "ok"
    assert _dumped_value(result.stdout, "FROM_FILE") is None, (
        f"the OCI tier must never read ocx.toml; stdout:\n{result.stdout}"
    )


def test_package_exec_env_flag_survives_generated_entrypoint_launcher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A `--env` override reaches a tool invoked THROUGH a generated launcher.

    A package that declares entrypoints resolves through its launcher, which
    re-enters `ocx launcher exec` — a process with no project context that
    rebuilds its env from scratch and re-applies the package's own entries on
    top. Without `set_forwarded_env` on this command the override is silently
    reverted at that hop.

    The override MUST target a key the package itself declares. A key the
    package does not declare survives the hop by plain inheritance whether or
    not it was forwarded, so a test using one passes either way and proves
    nothing — that is exactly the shape this assertion avoids.
    """
    from src.helpers import make_package_with_entrypoints

    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
            },
            {
                "key": "LAUNCHER_PROBE",
                "type": "constant",
                "value": "package-value",
            },
        ],
    )
    ocx.plain("package", "install", pkg.short)

    result = subprocess.run(
        [str(ocx.binary), "package", "exec", "--env", "LAUNCHER_PROBE=flag-value", pkg.short, "--", "showenv"],
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert _dumped_value(result.stdout, "LAUNCHER_PROBE") == "flag-value", (
        f"the override must survive the launcher re-entry — without forwarding "
        f"the launcher re-applies the package's own 'package-value' on top; "
        f"stdout:\n{result.stdout}"
    )


def test_package_tier_env_flag_rejects_reserved_and_bad_type(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The key and type gates apply identically on this tier — the flag is one
    parser, so `OCX_*` cannot be set and an unknown `:TYPE` is exit 64.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])
    ocx.plain("package", "install", pkg.short)

    for argument in ("OCX_DEFAULT_REGISTRY=evil.example.com", "X:bogus=v", "X:=v"):
        for command in (
            ["package", "exec", "--env", argument, pkg.short, "--", "hello"],
            ["package", "env", "--env", argument, pkg.short],
        ):
            result = subprocess.run(
                [str(ocx.binary), *command],
                capture_output=True,
                text=True,
                env=ocx.env,
                check=False,
            )
            assert result.returncode == EXIT_USAGE, (
                f"--env {argument} on `ocx {' '.join(command[:2])}` must exit "
                f"{EXIT_USAGE}; got {result.returncode}\nstderr:\n{result.stderr}"
            )


# ---------------------------------------------------------------------------
# Interpolation token grammar — `adr_interpolation_token_grammar.md`
#
# OCX claims every `${…}` (D3), so a token it does not recognise is an
# expansion error rather than text passed through. D14 decides *when* that
# error fires: reading a package never refuses, running or publishing one
# does. The legs below assert both halves against ONE document — a read-only
# leg with no failing sibling proves nothing, and a leg that only ever fails
# is indistinguishable from one that always fails.
#
# `ocx env` / `ocx exec` / `ocx install` in the ADR's prose are the OCI-tier
# spellings `ocx package env` / `package exec` / `package install`: the root
# forms either take no package (toolchain tier) or no longer exist.
# ---------------------------------------------------------------------------

# Stands in for "a token published by a newer ocx than this one". OCX has no
# `workspaceFolder` root and never will — it is VS Code's — so it can never
# drift into the recognised set and quietly stop exercising the unknown path.
UNRECOGNISED_TOKEN = "${workspaceFolder}"


def _token_document() -> dict:
    """The one metadata document every D14 leg is asserted against.

    Its single env value carries an unescaped `${workspaceFolder}`. Published
    it is unrunnable but readable (C-036, C-038); authored it is refused
    outright (C-037).
    """
    return {
        "type": "bundle",
        "version": 1,
        "env": [
            {
                "key": "EDITOR_ROOT",
                "type": "constant",
                "value": UNRECOGNISED_TOKEN,
                "visibility": "public",
            }
        ],
    }


def _list_tags(registry: str, repo: str) -> set[str]:
    """Tags the registry holds for ``repo`` — empty when it was never created."""
    try:
        with urllib.request.urlopen(f"http://{registry}/v2/{repo}/tags/list", timeout=5) as response:
            return set(json.loads(response.read()).get("tags") or [])
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return set()
        raise


def _content_tree(tmp_path: Path) -> Path:
    """A minimal bundleable content tree (one executable under ``bin/``)."""
    pkg_dir = tmp_path / "token-pkg"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)
    script = bin_dir / "app"
    script.write_text("#!/bin/sh\necho app\n")
    script.chmod(0o755)
    return pkg_dir


@pytest.fixture
def published_token_package(ocx: OcxRunner, unique_repo: str) -> str:
    """Publish `_token_document()` bypassing ocx's own publish gate.

    `ocx package create` / `push` refuse this document (that refusal is
    C-037's subject), so the fixture writes the wire shape directly — the
    house pattern `test_metadata_forward_compat.py` uses for metadata a live
    gate rejects. That is exactly the situation D14 exists for: an artifact
    minted by an ocx that knew a token this one does not.
    """
    layer_buffer = io.BytesIO()
    with tarfile.open(fileobj=layer_buffer, mode="w:xz") as tar:
        body = b"#!/bin/sh\necho app\n"
        info = tarfile.TarInfo(name="bin/app")
        info.size = len(body)
        info.mode = 0o755
        tar.addfile(info, io.BytesIO(body))

    os_name, architecture = current_platform().split("/")
    push_raw_package(
        ocx.registry,
        unique_repo,
        "1.0.0",
        _token_document(),
        layer_buffer.getvalue(),
        platform=(os_name, architecture),
    )
    ocx.plain("index", "update", f"{unique_repo}:1.0.0")
    return f"{unique_repo}:1.0.0"


def test_unrecognised_token_installs_and_inspects_but_refuses_to_compose(
    ocx: OcxRunner, published_token_package: str
) -> None:
    """Looking at a package never becomes impossible because it is too new;
    running it does (D14, C-036 + C-038, S-026).

    Three legs, one document:

    - `package pull` / `package install` succeed — the token check is off the
      ingress path, so the object lands on disk (C-038). Without this the
      read-only leg below would have nothing to read.
    - `package inspect --resolve` succeeds and echoes the value byte-for-byte
      (C-036, read side).
    - `package env` / `package exec` fail with exit 65 naming the token
      (C-036, run side).

    C-036 also names `ocx package info` on the read side; it is deliberately
    not asserted here, because no state of this package can make it fail.
    `package info` reads only the `__ocx.desc` tag and never the metadata, so
    it exits 0 even for a package that does not exist, and its JSON key is the
    CLI argument echoed back — both halves of a `rc == 0 and short in output`
    check hold on every successful invocation regardless. `inspect` is the
    falsifiable half of the read-side promise, and it is the one asserted.
    """
    short = published_token_package

    for read_write in (("package", "pull", short), ("package", "install", short)):
        result = ocx.run(*read_write, check=False, format=None)
        assert result.returncode == 0, (
            f"`ocx {' '.join(read_write)}` must succeed on a package carrying an "
            f"unrecognised token; got rc={result.returncode}\nstderr:\n{result.stderr}"
        )

    inspected = ocx.json("package", "inspect", "--resolve", short)
    values = [entry["value"] for entry in inspected["packages"][0]["metadata"]["env"]]
    assert values == [UNRECOGNISED_TOKEN], (
        f"`ocx package inspect` must echo the declared value verbatim — no expansion, "
        f"no elision; got {values!r}"
    )

    for composing in (("package", "env", short), ("package", "exec", short, "--", "app")):
        result = ocx.run(*composing, check=False, format=None)
        assert result.returncode == EXIT_DATA_ERR, (
            f"`ocx {' '.join(composing)}` must refuse the unrecognised token with exit "
            f"{EXIT_DATA_ERR}; got rc={result.returncode}\nstderr:\n{result.stderr}"
        )
        assert UNRECOGNISED_TOKEN in result.stderr, (
            f"the refusal must name the offending token; stderr:\n{result.stderr}"
        )


def test_unrecognised_token_is_refused_before_anything_reaches_the_registry(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx package create` and `ocx package push` refuse the same document
    the read path tolerates, exit 65, and leave the registry untouched
    (C-037, S-003, S-027).

    The refusal is the explicit publish gate, not `ValidMetadata::try_from` —
    which D14 moved off every ingress path, so a gate that was silently
    dropped would let this document reach a registry and strand every
    consumer of it.

    `push` is exercised against a sidecar `ocx package create` itself wrote
    (from a clean document) with only the env value swapped, so the push is
    rejected for the token and not for a hand-written metadata file the
    command would refuse anyway.
    """
    content = _content_tree(tmp_path)
    platform = current_platform()

    authored = tmp_path / "authored-metadata.json"
    authored.write_text(json.dumps(_token_document()))
    refused_bundle = tmp_path / "refused.tar.xz"
    created = ocx.run(
        "package", "create",
        "-m", str(authored),
        "-o", str(refused_bundle),
        "-p", platform,
        str(content),
        check=False,
        format=None,
    )
    assert created.returncode == EXIT_DATA_ERR, (
        f"`ocx package create` must refuse an unrecognised token with exit {EXIT_DATA_ERR}; "
        f"got rc={created.returncode}\nstderr:\n{created.stderr}"
    )
    assert UNRECOGNISED_TOKEN in created.stderr, (
        f"the refusal must name the offending token; stderr:\n{created.stderr}"
    )
    assert not refused_bundle.exists(), (
        f"a refused `package create` must leave no bundle behind: {refused_bundle}"
    )

    clean = _token_document()
    clean["env"][0]["value"] = "no-token-here"
    clean_path = tmp_path / "clean-metadata.json"
    clean_path.write_text(json.dumps(clean))
    bundle = tmp_path / "clean.tar.xz"
    ocx.plain(
        "package", "create", "-m", str(clean_path), "-o", str(bundle), "-p", platform, str(content)
    )
    sidecar = resolved_metadata_path(bundle)
    sidecar_document = json.loads(sidecar.read_text())
    sidecar_document["env"][0]["value"] = UNRECOGNISED_TOKEN
    sidecar.write_text(json.dumps(sidecar_document))

    repository = f"{unique_repo}_publish_gate"
    pushed = ocx.run(
        "package", "push",
        "-p", platform,
        "-m", str(sidecar),
        "-n", "--cascade",
        "-i", f"{ocx.registry}/{repository}:1.0.0",
        str(bundle),
        check=False,
        format=None,
    )
    assert pushed.returncode == EXIT_DATA_ERR, (
        f"`ocx package push` must refuse an unrecognised token with exit {EXIT_DATA_ERR}; "
        f"got rc={pushed.returncode}\nstderr:\n{pushed.stderr}"
    )
    assert UNRECOGNISED_TOKEN in pushed.stderr, (
        f"the refusal must name the offending token; stderr:\n{pushed.stderr}"
    )
    assert _list_tags(ocx.registry, repository) == set(), (
        f"a refused push must publish nothing; registry holds "
        f"{_list_tags(ocx.registry, repository)} under {repository}"
    )


def test_unrecognised_token_in_a_path_var_is_named_before_the_libc_lint_runs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The publish gate runs ahead of the libc lint, so a misspelt token in a
    `PATH` value is reported as a misspelt token (`package_create.rs`).

    The refusal above uses a `constant` var, which leaves the lint's scan scope
    empty — its ordering is unobservable there, and moving the gate below the
    lint keeps both the exit code and the stderr identical. An **interface**
    `Path` var is the shape that discriminates: the lint resolves its scan
    scope out of that same value, an unrecognised token is not a directory it
    can name, and it lands on `unresolvable`. Both orders exit 65 and both name
    the offending text, so the assertion that carries the contract is the
    *absence* of the scan-scope complaint.

    `linux/amd64` explicitly rather than the host platform: the lint is scoped
    to Linux and `any`, so on a macOS or Windows runner it would early-return
    and the leg would assert nothing.
    """
    content = _content_tree(tmp_path)
    document = {
        "type": "bundle",
        "version": 1,
        "env": [
            {
                "key": "PATH",
                "type": "path",
                "value": f"{UNRECOGNISED_TOKEN}/bin",
                "visibility": "interface",
            }
        ],
    }
    authored = tmp_path / "path-var-metadata.json"
    authored.write_text(json.dumps(document))
    bundle = tmp_path / f"{unique_repo}-path-var.tar.xz"

    created = ocx.run(
        "package", "create",
        "-m", str(authored),
        "-o", str(bundle),
        "-p", "linux/amd64",
        str(content),
        check=False,
        format=None,
    )

    assert created.returncode == EXIT_DATA_ERR, (
        f"`ocx package create` must refuse the token with exit {EXIT_DATA_ERR}; "
        f"got rc={created.returncode}\nstderr:\n{created.stderr}"
    )
    assert UNRECOGNISED_TOKEN in created.stderr, (
        f"the refusal must name the offending token; stderr:\n{created.stderr}"
    )
    assert "cannot resolve which directory" not in created.stderr, (
        "the publish gate must speak first: a misspelt token reported as an unresolvable "
        f"scan scope sends the publisher to fix the wrong thing\nstderr:\n{created.stderr}"
    )
    assert not bundle.exists(), (
        f"a refused `package create` must leave no bundle behind: {bundle}"
    )


def test_self_env_reference_resolves_identically_on_both_surfaces(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`${self.env.KEY}` reads the declaring package's own env, so the value a
    consumer sees never depends on which surface was asked for (C-024).

    The dependency declares a **private** `SEED` and an **interface**
    `DERIVED = ${self.env.SEED}`. On a dependency edge a carrier crosses on
    `has_interface()` regardless of the surface, so `DERIVED` crosses both and
    `SEED` crosses neither — the two runs then differ only in the surface
    asked for, which is what the contract is about. (At the root the same
    fixture would collapse: an interface carrier does not cross the private
    surface at all, leaving nothing to compare.)

    Equality alone is not the check. An implementation resolving
    `${self.env.SEED}` to the empty string on *both* surfaces satisfies it
    perfectly, so the agreed value is also asserted literally against `SEED`'s
    own declared value. The root's private `APP_PRIVATE` is the discriminator
    that the two invocations really selected different surfaces rather than
    running the same one twice.
    """
    seed_value = "seed-value-from-the-dependency"
    dependency = make_package(
        ocx,
        f"{unique_repo}_dep",
        "1.0.0",
        tmp_path,
        env=[
            {"key": "SEED", "type": "constant", "value": seed_value, "visibility": "private"},
            {
                "key": "DERIVED",
                "type": "constant",
                "value": "${self.env.SEED}",
                "visibility": "interface",
            },
        ],
    )
    digest = fetch_platform_manifest_digest(ocx.registry, dependency.repo, dependency.tag)
    application = make_package(
        ocx,
        f"{unique_repo}_app",
        "1.0.0",
        tmp_path,
        env=[
            {
                "key": "APP_PRIVATE",
                "type": "constant",
                "value": "app-private",
                "visibility": "private",
            }
        ],
        dependencies=[{"identifier": f"{dependency.fq}@{digest}", "visibility": "public"}],
    )
    ocx.plain("package", "install", application.short)

    surfaces = {}
    for flags in ((), ("--self",)):
        report = ocx.json("package", "env", *flags, application.short)
        surfaces[flags] = {entry["key"]: entry["value"] for entry in report["entries"]}

    consumer, private = surfaces[()], surfaces[("--self",)]
    assert "APP_PRIVATE" not in consumer and private.get("APP_PRIVATE") == "app-private", (
        f"the two runs must select different surfaces, or their agreement proves nothing; "
        f"consumer={consumer}, private={private}"
    )
    assert consumer.get("DERIVED") == private.get("DERIVED"), (
        f"`${{self.env.SEED}}` must resolve identically on both surfaces; "
        f"consumer={consumer.get('DERIVED')!r}, private={private.get('DERIVED')!r}"
    )
    assert consumer.get("DERIVED") == seed_value, (
        f"the agreed value must be SEED's own resolved value, not a uniformly degenerate "
        f"one; got {consumer.get('DERIVED')!r}, expected {seed_value!r}"
    )


def test_self_install_path_alias_resolves_like_the_bare_spelling(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`${self.installPath}` publishes and composes exactly like the bare
    `${installPath}` it aliases (S-001).

    Both spellings sit in the same document, so the assertion is an equality
    between two values the same command resolved — a resolver that quietly
    dropped the alias would leave the two apart.
    """
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        env=[
            {
                "key": "ALIAS_BIN",
                "type": "constant",
                "value": "${self.installPath}/bin",
                "visibility": "public",
            },
            {
                "key": "BARE_BIN",
                "type": "constant",
                "value": "${installPath}/bin",
                "visibility": "public",
            },
        ],
    )
    ocx.plain("package", "install", pkg.short)

    values = {entry["key"]: entry["value"] for entry in ocx.json("package", "env", pkg.short)["entries"]}
    assert values["ALIAS_BIN"] == values["BARE_BIN"], (
        f"`${{self.installPath}}` must resolve identically to `${{installPath}}`; got "
        f"{values['ALIAS_BIN']!r} vs {values['BARE_BIN']!r}"
    )
    assert values["BARE_BIN"].endswith("bin") and "${" not in values["BARE_BIN"], (
        f"both spellings must have expanded to the content tree; got {values['BARE_BIN']!r}"
    )


def test_escaped_token_publishes_and_composes_as_a_literal(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`$${workspaceFolder}` is the authoring path for a token meant for a
    downstream consumer: it publishes, and OCX emits the literal
    `${workspaceFolder}` for that consumer to expand (S-022, D2).

    Failing sibling of
    `test_unrecognised_token_installs_and_inspects_but_refuses_to_compose`:
    the same bytes, one escape apart, must be publishable here and refused
    there — which is what tells a working escape from a resolver that has
    simply stopped claiming `${…}` at all.
    """
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        env=[
            {
                "key": "EDITOR_ROOT",
                "type": "constant",
                "value": f"${UNRECOGNISED_TOKEN}",
                "visibility": "public",
            }
        ],
    )
    ocx.plain("package", "install", pkg.short)

    values = {entry["key"]: entry["value"] for entry in ocx.json("package", "env", pkg.short)["entries"]}
    assert values["EDITOR_ROOT"] == UNRECOGNISED_TOKEN, (
        f"the escape must collapse `$${{…}}` to a literal `${{…}}`; got "
        f"{values['EDITOR_ROOT']!r}, expected {UNRECOGNISED_TOKEN!r}"
    )
