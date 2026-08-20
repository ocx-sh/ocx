from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from uuid import uuid4

from src.runner import OcxRunner, PackageInfo, current_platform, registry_dir

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
COMPOSE_FILE = Path(__file__).resolve().parent.parent / "docker-compose.yml"


def env_key(name: str) -> str:
    """Fold a package display name or repository into an env-var key fragment.

    Namespaced names (`kitware/cmake`) are the norm now, so the separator has to
    fold like any other: `PKG_KITWARE_CMAKE`, not the `PKG_KITWARE/CMAKE` that a
    shell cannot name. Callers that derive the runtime `$PKG_*` / `$REPO_*`
    namespace and the ones that render the *declared* namespace must agree
    exactly — DE6 compares them — so both go through here rather than repeating
    the fold.
    """
    return name.upper().replace("/", "_").replace("-", "_")

# Sentinel distinguishing "caller passed nothing" from "caller passed None" /
# an explicit list for `make_package(binaries=...)`. The wire format
# deliberately gives `Option<Binaries>` absent (undeclared) and `Some([])`
# (declared empty) different meanings (adr_declared_binaries_metadata.md §1),
# so the helper needs a third state beyond Python's `None`.


class _Unset:
    pass


_UNSET = _Unset()

# ---------------------------------------------------------------------------
# Docker-compose helpers
# ---------------------------------------------------------------------------


def registry_is_reachable(registry: str) -> bool:
    """Return True if the registry responds to ``GET /v2/``."""
    try:
        urllib.request.urlopen(f"http://{registry}/v2/", timeout=2)
        return True
    except (urllib.error.URLError, OSError):
        return False


def start_registry(registry: str) -> None:
    """Start the registry via docker-compose if it is not already running."""
    if registry_is_reachable(registry):
        return

    subprocess.run(
        ["docker", "compose", "-f", str(COMPOSE_FILE), "up", "-d"],
        check=True,
        capture_output=True,
    )

    # Wait for the registry to become reachable (up to 15 s).
    for _ in range(30):
        if registry_is_reachable(registry):
            return
        time.sleep(0.5)

    raise RuntimeError(f"Registry at {registry} did not become reachable")


SIGSTORE_DIR = COMPOSE_FILE.parent / "sigstore"
_WAIT_FOR_STACK = SIGSTORE_DIR / "wait-for-stack.py"


#: Host-wide lock serializing compose bring-up across concurrent sessions.
#: Outside any worktree: two worktrees are exactly the case it exists for.
_COMPOSE_LOCK = Path.home() / ".cache" / "ocx-compose.lock"


def _compose(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["docker", "compose", "-f", str(COMPOSE_FILE), "--profile", "sigstore", *args],
        capture_output=True,
        text=True,
    )


def _stack_is_already_running() -> bool:
    """True when every service the profile declares is already running.

    The declared set comes from compose itself rather than a list here, so a
    service added to the profile is covered without a second edit. Other
    containers sharing the project name are ignored — this asks whether ours
    are up, not whether the daemon is idle.
    """
    declared = _compose("config", "--services")
    running = _compose("ps", "--services", "--filter", "status=running")
    if declared.returncode != 0 or running.returncode != 0:
        return False  # cannot prove it is up, so fall through to `up -d`
    return set(declared.stdout.split()) <= set(running.stdout.split())


def start_sigstore_stack(timeout: float = 300.0) -> None:
    """Bring up the `sigstore` compose profile and block until it answers.

    Readiness is delegated to ``sigstore/wait-for-stack.py`` rather than
    reimplemented here. That script is also what the self-hosting documentation
    hands an operator, so a second copy of the endpoint list is the one that
    would go stale — and it is the copy the suite would be trusting.

    Two guards make this safe to call from concurrent sessions, which is the
    normal case whenever more than one worktree runs the suite:

    * **Skip when it is already up.** Every worktree resolves the same compose
      project name (``test``), but their bind-mount paths differ, so the config
      hash differs and an ``up -d`` from one worktree *recreates* the other's
      containers — mid-suite, as connection-refused on every port. Not issuing
      the command at all is what removes that; the lock alone cannot, because
      the recreation is legal work correctly serialized.
    * **Serialize the rest.** Two simultaneous ``up -d`` calls race on creating
      the same container and one loses outright (``dex exited (2)``,
      ``No such container: <hash>_test-registry-1``).

    The readiness wait stays outside the lock: it is a read-only poll, and
    holding an exclusive lock across a 300s timeout would serialize sessions
    that have nothing left to contend over.
    """
    _bring_up_serialized()
    done = subprocess.run(
        [sys.executable, str(_WAIT_FOR_STACK), "--timeout", str(timeout)],
        capture_output=True,
        text=True,
    )
    if done.returncode != 0:
        raise RuntimeError(
            "the sigstore compose profile did not come up:\n"
            + done.stdout
            + done.stderr
        )


def _bring_up_serialized() -> None:
    """Check-then-``up`` under an exclusive host lock, so the two are atomic."""
    try:
        import fcntl
    except ImportError:  # Windows: no flock, and no shared stack to contend for
        if not _stack_is_already_running():
            _require_up(_compose("up", "-d"))
        return

    _COMPOSE_LOCK.parent.mkdir(parents=True, exist_ok=True)
    with open(_COMPOSE_LOCK, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)  # blocking: a peer's `up` is worth waiting for
        try:
            if not _stack_is_already_running():
                _require_up(_compose("up", "-d"))
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


def _require_up(result: subprocess.CompletedProcess[str]) -> None:
    if result.returncode != 0:
        raise RuntimeError(
            "`docker compose up -d` failed for the sigstore profile:\n"
            + result.stdout
            + result.stderr
        )


#: The dex identity the local stack mints, and the `iss` claim it writes.
#: In-network issuer URL: the host fetches over localhost, but the claim names
#: the address Fulcio resolves. ocx never dials the issuer, it compares strings.
SIGSTORE_IDENTITY = "ocx-test@example.com"
SIGSTORE_ISSUER = "http://dex:5556/dex"


def mint_identity_token(target: Path) -> Path:
    """Write a fresh dex OIDC token to ``target`` and return it.

    `ocx package sign` has no `--identity-token <VALUE>` flag by design — a
    token in argv is world-readable in /proc — so the token reaches it as a
    file, and the file must not be group- or world-readable or sign refuses it.
    """
    token = subprocess.run(
        [
            sys.executable,
            str(SIGSTORE_DIR / "get-token.py"),
            "--port",
            os.environ.get("OCX_TEST_DEX_PORT", "5556"),
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if token.count(".") != 2:
        raise RuntimeError(f"dex did not return a JWT: {token[:80]!r}")
    target.write_text(token)
    target.chmod(0o600)
    return target


# ---------------------------------------------------------------------------
# Package publishing
# ---------------------------------------------------------------------------

# Mirrors `conventions::infer_metadata_file` in the Rust CLI: extensions
# checked after one suffix is already stripped by `Path.stem` (Python) /
# `Path::file_stem` (Rust).
_KNOWN_ARCHIVE_EXTENSIONS = (".tar", ".tar.gz", ".tgz", ".zip")


def resolved_metadata_path(bundle: Path) -> Path:
    """Path `ocx package create -o <bundle>` writes the canonicalized sidecar to.

    `ocx package create` never writes back to its `-m` input file — it always
    writes next to `-o` (see `conventions::infer_metadata_file`). Fields
    `create` resolves and records (dependency pins) only exist in *this*
    file; `push`/`package test` must read metadata from here, not from the
    pre-`create` input. The platform `create` resolved against is recorded
    separately, in the sibling build receipt — see `resolved_receipt_path`.
    """
    stem = bundle.stem
    for ext in _KNOWN_ARCHIVE_EXTENSIONS:
        if stem.endswith(ext):
            stem = stem[: -len(ext)]
            break
    return bundle.parent / f"{stem}-metadata.json"


def resolved_receipt_path(bundle: Path) -> Path:
    """Path `ocx package create -o <bundle>` writes the build receipt to.

    Same stem-stripping as `resolved_metadata_path`, `-receipt.json` suffix
    instead of `-metadata.json`. `create` writes it whenever it was told
    anything worth recording — `-p`, `-i`, or both, with or without `-m`:
    `{"version": 1, "platform": "<canonical>", "identifier": "<canonical>"}`,
    each of the two optional. The metadata sidecar itself carries no
    `platform` key (published wire shape). `push`/`package test` read it only
    for what their own flags did not supply.
    """
    stem = bundle.stem
    for ext in _KNOWN_ARCHIVE_EXTENSIONS:
        if stem.endswith(ext):
            stem = stem[: -len(ext)]
            break
    return bundle.parent / f"{stem}-receipt.json"


def _build_trap_script(outputs: dict[str, str], marker: str) -> str:
    """Build an argument-aware trap shell script.

    ``outputs`` maps argument strings to the exact output the binary should
    produce (e.g. ``{"--version": "uv 0.10.10"}``).  All values are emitted
    via single-quoted heredocs so JSON output containing ``"`` characters
    (e.g. ``{"version": "0.0.1"}``) survives untouched — embedding such
    values in a double-quoted ``echo`` argument would let the inner ``"``
    terminate the string and corrupt the payload.
    """
    lines = ["#!/bin/sh", 'case "$*" in']
    for args, output in outputs.items():
        lines.append(f'  "{args}")')
        lines.append("    cat <<'TRAP_EOF'")
        lines.append(output)
        lines.append("TRAP_EOF")
        lines.append("    ;;")
    # Fallback: emit marker for acceptance tests (also via heredoc so any
    # marker shape — currently ASCII-only, but defensive for future changes —
    # is byte-exact preserved).
    lines.append("  *)")
    lines.append("    cat <<'TRAP_EOF'")
    lines.append(marker)
    lines.append("TRAP_EOF")
    lines.append("    ;;")
    lines.append("esac")
    return "\n".join(lines) + "\n"


def inspect_entry(payload: dict, name: str) -> dict:
    """Pick one entry out of an inspect report's ``packages`` array by ``name``.

    Both inspect commands emit ``{platform, packages: [...], env: [...]}`` where
    each entry names itself: the raw request string for ``ocx package inspect``,
    the ``ocx.toml`` binding for ``ocx inspect``. The array preserves order
    (input order / selection order), which a JSON object keyed by request string
    could not promise.
    """
    matches = [entry for entry in payload["packages"] if entry["name"] == name]
    assert matches, (
        f"no inspect entry named {name!r}; "
        f"got {[entry['name'] for entry in payload['packages']]}"
    )
    assert len(matches) == 1, f"inspect entry {name!r} appears {len(matches)} times"
    return matches[0]


def inspect_names(payload: dict) -> list[str]:
    """The ``name`` of every entry in an inspect report, in emitted order."""
    return [entry["name"] for entry in payload["packages"]]


# ---------------------------------------------------------------------------
# Project files
# ---------------------------------------------------------------------------


def write_ocx_toml(project_dir: Path, body: str) -> Path:
    """Write ``body`` to ``project_dir/ocx.toml`` and return the file's path."""
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


# ---------------------------------------------------------------------------
# Deferred (lazy) tools — the shim store
# ---------------------------------------------------------------------------


def shim_bin_dirs(ocx: OcxRunner, repo: str) -> list[Path]:
    """Every published shim launcher directory belonging to ``repo``.

    A shim directory lives at
    ``$OCX_HOME/shims/<registry-slug>/<repo-path>/<algo>/<2hex>/<30hex>/`` and
    is complete exactly when its ``bin/`` child exists — the same marker the
    producer writes last and the GC walker classifies on. The repository IS in
    this path (unlike the package store, which keys on registry + digest
    alone), so scoping the walk to one repository is enough to identify a
    package's shims without knowing its digest.

    ``ocx clean`` removes the shim directory but leaves its now-empty CAS
    ancestors behind, so absence must be read off ``bin/``, never off the
    repository subtree.
    """
    root = ocx.ocx_home / "shims" / registry_dir(ocx.registry)
    for segment in repo.split("/"):
        root = root / segment
    if not root.is_dir():
        return []
    return sorted(path for path in root.rglob("bin") if path.is_dir())


def assert_shim_dir_exists(ocx: OcxRunner, repo: str, why: str) -> Path:
    """Assert ``repo`` owns exactly one shim tree, and return its ``bin/``.

    Exactly one, not "at least one": a test publishes a single version per
    repository, so a second tree means a stale digest was left behind rather
    than converged on.
    """
    found = shim_bin_dirs(ocx, repo)
    assert len(found) == 1, (
        f"{why}: expected exactly one shim `bin/` for {repo} under "
        f"{ocx.ocx_home / 'shims'}; found {[str(p) for p in found]}"
    )
    return found[0]


def assert_shim_dir_absent(ocx: OcxRunner, repo: str, why: str) -> None:
    """Assert ``repo`` owns no shim tree.

    Only meaningful when the same test has already seen
    :func:`assert_shim_dir_exists` succeed for ``repo`` — otherwise an empty
    result is indistinguishable from a probe looking in the wrong place.
    """
    found = shim_bin_dirs(ocx, repo)
    assert found == [], (
        f"{why}: {repo} must own no shim `bin/`; found {[str(p) for p in found]}"
    )


def make_package(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    tmp_path: Path,
    *,
    new: bool = True,
    cascade: bool = True,
    index: bool = True,
    size_mb: int = 0,
    layers: int = 1,
    platform: str | None = None,
    bins: list[str] | None = None,
    env: list[dict] | None = None,
    outputs: dict[str, dict[str, str]] | None = None,
    bin_scripts: dict[str, str] | None = None,
    dependencies: list[dict] | None = None,
    binaries: list[str] | _Unset = _UNSET,
    bin_exec: dict[str, bool] | None = None,
    no_bin_scan: bool = False,
    extra_push_args: list[str] | None = None,
    integrations: dict[str, object] | None = None,
) -> PackageInfo:
    """Create, bundle, push, and index a test package.

    Parameters
    ----------
    size_mb:
        Approximate size in MB of random padding data.  Useful for making
        downloads large enough to show progress bars.  When ``layers > 1``
        the total padding is divided across the layers (``size_mb // layers``
        MB per layer).
    layers:
        Number of OCI layers to split the package into.  Defaults to 1
        (single-bundle push, existing behaviour).  When > 1, N separate
        ``tar.xz`` archives are built, each with a distinct content path
        (``layer_N/data.bin``) so the blobs are never identical, then all
        are passed to ``ocx package push`` as positional layer arguments.
        Binaries and metadata always reside in layer 0 (``bundle-...-L0``).
    platform:
        OCI platform string (e.g. ``linux/amd64``).  Defaults to the
        current host platform.
    bins:
        List of binary names to create.  Each gets a shell script that
        echoes a unique marker.  Defaults to ``["hello"]``.
    env:
        Custom metadata env entries.  Defaults to PATH + ``{REPO}_HOME``
        (derived from the repo name, e.g. ``cmake`` → ``CMAKE_HOME``).

        Each item is a dict accepting arbitrary keys; the v2 schema field
        ``"visibility"`` (one of ``"private"`` (default) | ``"public"`` |
        ``"interface"``) controls which ``--mode`` includes the entry. See
        ``adr_visibility_two_axis_and_exec_modes.md`` Tension 1 + 3.
    outputs:
        Maps binary name to ``{args: output}`` pairs.  When provided, the
        trap binary uses a ``case`` block to reproduce exact command output
        for specific argument patterns.  Multi-line output uses heredocs.
    bin_scripts:
        Maps binary name to a literal shell script body.  When provided for
        a name, the exact content is written verbatim (and made executable)
        instead of the generated trap script.  Takes precedence over
        ``outputs`` for the same name.  Ignored on Windows (``outputs``
        behaviour is Windows-incompatible anyway).
    binaries:
        Sets the metadata sidecar's ``binaries`` claim field.  Omit entirely
        (default) to leave the field undeclared; pass a list (including
        ``[]``) to declare it explicitly — the two are distinct wire states
        (``adr_declared_binaries_metadata.md`` §1).  Passed straight through
        to ``ocx package create``, so scan/verify behaviour follows whatever
        ``--bin-scan``/``--no-bin-scan`` semantics are in effect (default
        Auto: a declared field is never scanned or verified, only an absent
        one is filled).
    bin_exec:
        Maps binary name to whether that bundled file gets the executable
        bit (``0o755``).  Defaults to executable for every name in ``bins``
        when omitted.  Ignored on Windows (no exec-bit concept — scan there
        uses an extension allowlist instead).
    no_bin_scan:
        Passes ``--no-bin-scan`` to ``ocx package create``, so the default
        Auto-mode scan never fills a ``binaries`` claim even when ``bin/``
        holds an interface-visible executable.  Use this to build a
        genuinely claim-less (pre-ADR / legacy-shaped) fixture package.
    extra_push_args:
        Extra flags appended to the ``ocx package push`` invocation (e.g.
        ``["--no-canonical-tag"]``), after ``-n``/``--cascade`` and before
        ``-i``.
    integrations:
        Sets the metadata sidecar's ``integrations`` map (namespace ->
        opaque JSON payload, `adr_package_integrations.md`). Omit entirely
        (default) to leave the field undeclared — absent and empty are the
        same wire state (D13), unlike ``binaries``. Passed straight through
        to ``ocx package create``, unresolved (interpolation tokens survive
        verbatim until composed).
    """
    plat = platform or current_platform()
    marker = f"marker-{uuid4().hex[:12]}"
    bin_names = bins or ["hello"]

    # Sanitize repo for use in filesystem paths: replace `/` and other
    # path-separator characters so pathlib does not treat the segment as
    # a subdirectory. The repo value is still passed unchanged to ocx
    # package commands (OCI identifier semantics).
    fname_slug = repo.replace("/", "_")

    # Build content
    pkg_dir = tmp_path / f"pkg-{fname_slug}-{tag}"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)

    for name in bin_names:
        script = bin_dir / name
        bin_outputs = (outputs or {}).get(name)
        bin_body = (bin_scripts or {}).get(name)
        executable = (bin_exec or {}).get(name, True)
        if sys.platform == "win32":
            script = script.with_suffix(".bat")
            script.write_text(f"@echo {marker}\n")
        elif bin_body is not None:
            script.write_text(bin_body)
            if executable:
                script.chmod(script.stat().st_mode | stat.S_IEXEC)
        elif bin_outputs:
            script.write_text(_build_trap_script(bin_outputs, marker))
            if executable:
                script.chmod(script.stat().st_mode | stat.S_IEXEC)
        else:
            script.write_text(f"#!/bin/sh\necho {marker}\n")
            if executable:
                script.chmod(script.stat().st_mode | stat.S_IEXEC)

    # Add random padding for realistic download sizes.
    # For single-layer packages all padding lives in pkg_dir/lib/data.bin.
    # For multi-layer packages the padding is omitted from pkg_dir — it goes
    # into the extra layer directories built below.
    n_layers = max(1, layers)
    if size_mb > 0 and n_layers == 1:
        lib_dir = pkg_dir / "lib"
        lib_dir.mkdir(parents=True)
        data_file = lib_dir / "data.bin"
        data_file.write_bytes(os.urandom(size_mb * 1024 * 1024))

    # Write metadata
    metadata_path = tmp_path / f"metadata-{fname_slug}-{tag}.json"
    home_key = env_key(repo) + "_HOME"
    # Default env is tagged ``"visibility": "public"`` so the per-entry
    # filter under ``ExecMode::Consumer`` (the default since the v2 flip)
    # admits PATH and {REPO}_HOME — matching the in-tree bare-binary mirror
    # convention (`mirrors/uv`, `mirrors/cpython`, etc.). Tests that want
    # to exercise default-`private` behaviour pass an explicit ``env=[...]``
    # without ``visibility`` keys.
    metadata_env = env or [
        {
            "key": "PATH",
            "type": "path",
            "required": True,
            "value": "${installPath}/bin",
            "visibility": "public",
        },
        {
            "key": home_key,
            "type": "constant",
            "value": "${installPath}",
            "visibility": "public",
        },
    ]
    metadata_obj: dict = {
        "type": "bundle",
        "version": 1,
        "env": metadata_env,
    }
    if dependencies:
        metadata_obj["dependencies"] = dependencies
    if binaries is not _UNSET:
        metadata_obj["binaries"] = binaries
    if integrations is not None:
        metadata_obj["integrations"] = integrations
    metadata_path.write_text(json.dumps(metadata_obj))

    # Create bundles.  For multi-layer packages, build one bundle per layer:
    # layer 0 contains the binaries+metadata tree (pkg_dir), extra layers
    # contain only a synthetic padding blob at a unique path so no two layers
    # share an identical blob.
    layer_per_mb = (size_mb // n_layers) if n_layers > 1 and size_mb > 0 else 0

    # Single-layer keeps the historical bundle filename — fixtures outside
    # this helper (e.g. test_package_test_script.py) construct that path by
    # convention; the -L0 suffix exists only when extra layers are built.
    bundle_l0 = tmp_path / (
        f"bundle-{fname_slug}-{tag}.tar.xz"
        if n_layers == 1
        else f"bundle-{fname_slug}-{tag}-L0.tar.xz"
    )
    ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata_path),
        "-o",
        str(bundle_l0),
        "-p",
        plat,
        *(["--no-bin-scan"] if no_bin_scan else []),
        str(pkg_dir),
    )

    all_bundles: list[Path] = [bundle_l0]
    for li in range(1, n_layers):
        layer_dir = tmp_path / f"layer-{fname_slug}-{tag}-L{li}"
        layer_dir.mkdir(parents=True)
        blob = layer_dir / f"layer_{li}" / "data.bin"
        blob.parent.mkdir(parents=True)
        blob.write_bytes(os.urandom(max(1, layer_per_mb) * 1024 * 1024))
        bundle_ln = tmp_path / f"bundle-{fname_slug}-{tag}-L{li}.tar.xz"
        ocx.plain(
            "package",
            "create",
            "-m",
            str(metadata_path),
            "-o",
            str(bundle_ln),
            "-p",
            plat,
            str(layer_dir),
        )
        all_bundles.append(bundle_ln)

    # Push — pass all layer bundles as positional args to `ocx package push`.
    # `-m` points at the sidecar `create` actually wrote (next to bundle_l0),
    # not the pre-`create` input — that's the file carrying `create`'s
    # resolved dependency pins.
    fq = f"{ocx.registry}/{repo}:{tag}"
    push_args = ["package", "push", "-p", plat, "-m", str(resolved_metadata_path(bundle_l0))]
    if new:
        push_args.append("-n")
    if cascade:
        push_args.append("--cascade")
    if extra_push_args:
        push_args += extra_push_args
    push_args += ["-i", fq] + [str(b) for b in all_bundles]
    ocx.plain(*push_args)

    # Update local index so install/find can discover the package.
    # When cascade is enabled, use bare repo name to index all cascaded tags;
    # when disabled, use tagged identifier for minimal indexing.
    #
    # `index=False` skips this step — needed when a replace-mirror is configured
    # that does not carry this repo, so the tag-refresh read would 404. `ocx
    # index update` now propagates that failure (a package-manager batch surfaces
    # its errors), so callers that only assert against the registry over HTTP and
    # never consult the local index must opt out.
    short = f"{repo}:{tag}"
    if index:
        index_target = repo if cascade else short
        ocx.plain("index", "update", index_target)

    return PackageInfo(
        repo=repo,
        tag=tag,
        short=short,
        fq=fq,
        content_dir=pkg_dir,
        marker=marker,
        platform=plat,
    )


def push_managed_config(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    config_toml: str,
    tmp_path: Path,
    *,
    cascade: bool = False,
    new: bool = True,
) -> str:
    """Publishes a managed-config payload via the real ``ocx config push``
    (the product path — prefer this over ``src.registry.push_raw_config_package``
    unless the test needs a malformed wire shape).

    Returns the pushed package's index digest (``sha256:<hex>``).
    """
    payload = tmp_path / f"managed-config-{uuid4().hex[:8]}.toml"
    payload.write_text(config_toml)
    args = ["config", "push", "-i", f"{ocx.registry}/{repo}:{tag}"]
    if cascade:
        args.append("--cascade")
    if new:
        args.append("--new")
    args.append(str(payload))
    report = ocx.json(*args)
    return report["manifest_digest"]


def make_package_with_entrypoints(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    entrypoints: list[str] | dict[str, dict],
    bins: list[str] | None = None,
    tag: str = "1.0.0",
    *,
    file_prefix: str = "ep",
    dependencies: list[dict] | None = None,
    env: list[dict] | None = None,
    extra_files: dict[str, str] | None = None,
    bin_exec: dict[str, bool] | None = None,
    integrations: dict[str, object] | None = None,
) -> PackageInfo:
    """Publish a test package whose metadata declares ``entrypoints``.

    Shared by ``test_entrypoints*`` modules so the suite has one construction
    path. ``file_prefix`` keeps temp filenames distinct when a single
    ``tmp_path`` hosts multiple packages.

    Parameters
    ----------
    entrypoints:
        Either a list of entrypoint names (each materializes as an empty
        value object) or a pre-built map of name -> value object. The on-wire
        shape is a JSON object keyed by name.  When passing a dict, per-entry
        value objects may carry any supported fields — including ``args`` for
        baked leading arguments (``{"args": ["${installPath}/script.py"]}``).
    dependencies:
        Optional list of dependency descriptors to embed in metadata.  Each
        entry must be a dict with at least an ``identifier`` key (same shape
        as ``make_package``'s ``dependencies`` parameter).
    env:
        Optional override for the metadata env entries. Defaults to a single
        ``PATH = ${installPath}/bin`` entry (no visibility — picks up the
        v2 default `private`). Pass an explicit list when a test needs
        consumer-visible PATH or other env shapes.
    extra_files:
        Optional mapping of POSIX-relative paths (from the package content
        root) to text content.  Each entry creates the file at
        ``pkg_dir/<rel_path>`` before bundling, so the file is present in the
        package's installed ``content/`` tree and reachable via
        ``${installPath}/<rel_path>`` at runtime.  Parent directories are
        created automatically.  Defaults to ``None`` (no extra files).
    bin_exec:
        Maps a binary name to whether it gets the executable bit (default:
        every name in ``bins`` does).  A ``False`` entry builds the shadow-rule
        fixture at the launcher hop: an entrypoint whose dispatch target the
        package ships but cannot run.  POSIX only — Windows has no exec bit.
    integrations:
        Sets the metadata sidecar's ``integrations`` map. See
        ``make_package``'s parameter of the same name.
    """
    if isinstance(entrypoints, list):
        for n in entrypoints:
            if not isinstance(n, str):
                raise TypeError(
                    "entrypoints list must contain plain command-name strings "
                    f"(got element of type {type(n).__name__}); pass a dict[str, dict] "
                    "if per-entry value objects are needed"
                )
        entrypoints_obj: dict[str, dict] = {name: {} for name in entrypoints}
    else:
        entrypoints_obj = dict(entrypoints)
    bin_names = bins or ["hello"]
    if env is None:
        env = [
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
            },
        ]
    pkg_dir = tmp_path / f"pkg-{file_prefix}-{unique_repo}-{tag}"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)
    marker = f"marker-{uuid4().hex[:12]}"
    for name in bin_names:
        script = bin_dir / name
        if sys.platform == "win32":
            script = script.with_suffix(".bat")
            script.write_text(f"@echo entry-point-{name} {marker} %*\n")
        else:
            script.write_text(f'#!/bin/sh\necho "entry-point-{name} {marker} $@"\n')
            if (bin_exec or {}).get(name, True):
                script.chmod(script.stat().st_mode | stat.S_IEXEC)

    if extra_files:
        for rel_path, content in extra_files.items():
            dest = pkg_dir / rel_path
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(content)

    metadata_path = tmp_path / f"metadata-{file_prefix}-{unique_repo}-{tag}.json"
    metadata_obj: dict = {
        "type": "bundle",
        "version": 1,
        "env": env,
        "entrypoints": entrypoints_obj,
    }
    if dependencies:
        metadata_obj["dependencies"] = dependencies
    if integrations is not None:
        metadata_obj["integrations"] = integrations
    metadata_path.write_text(json.dumps(metadata_obj))

    bundle = tmp_path / f"bundle-{file_prefix}-{unique_repo}-{tag}.tar.xz"
    plat = current_platform()
    ocx.plain(
        "package", "create", "-m", str(metadata_path), "-o", str(bundle), "-p", plat, str(pkg_dir)
    )

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain(
        "package",
        "push",
        "-p",
        plat,
        "-m",
        str(resolved_metadata_path(bundle)),
        "-n",
        "--cascade",
        "-i",
        fq,
        str(bundle),
    )
    short = f"{unique_repo}:{tag}"
    ocx.plain("index", "update", unique_repo)

    return PackageInfo(
        repo=unique_repo,
        tag=tag,
        short=short,
        fq=fq,
        content_dir=pkg_dir,
        marker=marker,
        platform=plat,
    )
