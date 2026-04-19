"""Acceptance tests — `ocx exec` URI scheme: bare / oci:// / file://.

Replaces the prior ``--install-dir`` flag tests. The flag was removed in the
pre-release reshape on `feat/package-entry-points`; the new contract is a
single positional accepting `<bare>`, `oci://<id>`, or
`file://<absolute-package-root>`.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

from src.runner import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def get_install_content_path(ocx: OcxRunner, pkg: PackageInfo) -> Path | None:
    """Return the on-disk content/ path for an installed package, or None."""
    result = ocx.json("find", pkg.short)
    path = result.get(pkg.short) if isinstance(result, dict) else None
    return Path(path) if path else None


# ---------------------------------------------------------------------------
# file:// URI parity with identifier mode
# ---------------------------------------------------------------------------


def test_exec_file_uri_runs_command_against_installed_package(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """`ocx exec file://<pkg-root> -- echo hi` runs successfully against an installed package."""
    pkg = published_package
    ocx.plain("install", pkg.short)

    content_path = get_install_content_path(ocx, pkg)
    assert content_path is not None, (
        "ocx find must report a content path for an installed package"
    )
    pkg_root = content_path.parent

    result = ocx.run("exec", f"file://{pkg_root}", "--", "echo", "hi", check=False)
    assert result.returncode == 0, (
        f"file:// URI with valid absolute package root must succeed; "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert result.stdout.strip() == "hi", (
        f"file:// mode must forward the command and args verbatim; "
        f"got stdout={result.stdout.strip()!r}"
    )


def test_exec_file_uri_relative_path_rejected_with_usage_error(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """`ocx exec file://relative/path` must exit 64 (UsageError, not absolute)."""
    pkg = published_package
    ocx.plain("install", pkg.short)

    result = ocx.run("exec", "file://relative/path", "--", "echo", "hi", check=False)
    assert result.returncode == 64, (
        f"relative file:// URI must exit 64 (UsageError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert "absolute" in result.stderr.lower(), (
        f"error must mention 'absolute'; stderr={result.stderr.strip()}"
    )


def test_exec_identifier_mode_unchanged(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Regression guard: bare identifier mode still works after the reshape."""
    pkg = published_package
    ocx.plain("install", pkg.short)
    result = ocx.plain("exec", pkg.short, "--", "hello")
    assert result.returncode == 0, f"identifier-mode exec must still work; rc={result.returncode}"
    assert result.stdout.strip() == pkg.marker, (
        f"exec output must match marker; got: {result.stdout.strip()!r}"
    )


def test_exec_oci_scheme_prefix_accepted(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Explicit `oci://<id>` must parse equivalently to a bare identifier."""
    pkg = published_package
    ocx.plain("install", pkg.short)
    result = ocx.plain("exec", f"oci://{pkg.short}", "--", "hello")
    assert result.returncode == 0, (
        f"oci://<id> mode must succeed; rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert result.stdout.strip() == pkg.marker, (
        f"exec output must match marker; got: {result.stdout.strip()!r}"
    )


def test_file_uri_outside_packages_root_rejected_with_usage_error(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A `file://` URI pointing outside ``OCX_HOME/packages/`` must exit 64."""
    bogus = tmp_path / "outside-packages-root"
    bogus.mkdir()

    result = ocx.run("exec", f"file://{bogus}", "--", "echo", "hi", check=False)
    assert result.returncode == 64, (
        f"file:// URI outside OCX_HOME/packages/ must exit 64 (UsageError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert "file://" in result.stderr, (
        f"error must name the offending scheme 'file://'; "
        f"stderr={result.stderr.strip()}"
    )


def test_file_uri_missing_metadata_json_rejected(
    ocx: OcxRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A path under packages/ without metadata.json must be rejected as a non-package-root."""
    # Build a fake path that lives inside the OCX_HOME packages dir but has no metadata.json.
    fake = Path(ocx.ocx_home) / "packages" / "fake.example.com" / "sha256" / "ab" / "fake-pkg"
    fake.mkdir(parents=True, exist_ok=True)

    result = ocx.run("exec", f"file://{fake}", "--", "echo", "hi", check=False)
    assert result.returncode == 64, (
        f"file:// without metadata.json must exit 64 (UsageError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert "metadata.json" in result.stderr, (
        f"error must mention 'metadata.json'; stderr={result.stderr.strip()}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="printenv is POSIX-only")
def test_exec_file_uri_produces_same_env_as_identifier_mode(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """`ocx exec file://<pkg-root> -- printenv PATH` produces same PATH as identifier mode."""
    pkg = published_package
    ocx.plain("install", pkg.short)

    content_path = get_install_content_path(ocx, pkg)
    assert content_path is not None, (
        "ocx find must report a content path for an installed package"
    )
    pkg_root = content_path.parent

    id_result = ocx.plain("exec", pkg.short, "--", "printenv", "PATH")
    file_result = ocx.plain("exec", f"file://{pkg_root}", "--", "printenv", "PATH")

    assert id_result.stdout.strip() == file_result.stdout.strip(), (
        f"file:// mode must produce same PATH as identifier mode;\n"
        f"identifier: {id_result.stdout.strip()!r}\n"
        f"file://:   {file_result.stdout.strip()!r}"
    )
