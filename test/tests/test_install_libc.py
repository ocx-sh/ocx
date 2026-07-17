"""libc-aware install resolution acceptance tests (Step 3.6).

These tests exercise the full install path when the host's libc detection is
controlled via the ``__OCX_TEST_LIBC`` env-override hook (Living Design Record
amendment 2026-05-28).

__OCX_TEST_LIBC values (test-support only — not in user docs):
  "glibc" → detection returns Some(Glibc); Platform::current() sets os.features=["libc.glibc"]
  "musl"  → detection returns Some(Musl); Platform::current() sets os.features=["libc.musl"]
  "none"  → detection returns None; Platform::current() sets os.features=None
  unset   → real ld.so probe (never used in CI; requires real host)

Each test sets __OCX_TEST_LIBC on the ``OcxRunner.env`` dict and removes it when
done (via pytest fixture teardown).  The tests publish two libc-tagged entries
(plus an optional untagged fallback) and assert that install picks the correct
one.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

from src import OcxRunner, make_package, registry_dir
from src.assertions import assert_symlink_exists


def _installed_marker_via_seam(ocx: OcxRunner, short: str, libc: str) -> str:
    """Read the marker from the installed ``bin/hello`` binary.

    Uses ``__OCX_TEST_LIBC`` during ``package which`` so that the host
    detection is the same seam used during install; avoids having to pass an
    explicit ``--platform`` flag, which would break tests that exercise
    seam-driven selection (not explicit-platform selection).
    """
    try:
        ocx.env["__OCX_TEST_LIBC"] = libc
        which = ocx.json("package", "which", short)
    finally:
        ocx.env.pop("__OCX_TEST_LIBC", None)
    pkg_root = Path(which[short])
    hello = pkg_root / "content" / "bin" / "hello"
    return hello.read_text()

# ---------------------------------------------------------------------------
# Real-host markers (manual). The seam-driven tests above force the detected
# libc via __OCX_TEST_LIBC and run anywhere. The markers below run the REAL
# discovery-then-identify probe (no seam) and are skipped unless run on the
# named host with OCX_REAL_HOST_LIBC_TESTS set — they document the non-FHS
# expectations the FHS-only allowlist could not satisfy. See
# adr_platform_libc_os_features.md "Detection mechanism v2" and
# crates/ocx_lib/src/oci/host_capabilities.rs.
# ---------------------------------------------------------------------------

REAL_HOST_LIBC_ENV = "OCX_REAL_HOST_LIBC_TESTS"

requires_real_host = pytest.mark.skipif(
    os.environ.get(REAL_HOST_LIBC_ENV) is None,
    reason=(
        f"set {REAL_HOST_LIBC_ENV}=1 on a real NixOS / Gentoo Prefix host to run; "
        "exercises real PT_INTERP discovery (no __OCX_TEST_LIBC seam)"
    ),
)


# ---------------------------------------------------------------------------
# 3.6 — libc-aware install selection tests
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    sys.platform != "linux",
    reason="libc differentiation only applies to linux/amd64 hosts",
)
def test_install_selects_libc_glibc_on_glibc_host(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """ocx install on a glibc host selects the libc.glibc entry, not the musl one.

    Pushes a ``linux/amd64+libc.glibc`` entry and a ``linux/amd64+libc.musl``
    entry under the same tag, each with a distinct marker binary.  Then installs
    with ``__OCX_TEST_LIBC=glibc`` and asserts that the glibc marker is
    materialised — proving that libc detection drives entry selection, not a
    no-op that would install either variant.
    """
    import platform as _platform
    if _platform.machine().lower() not in {"x86_64", "amd64"}:
        pytest.skip("test fixtures target linux/amd64")

    glibc_build = tmp_path / "glibc-build"
    musl_build = tmp_path / "musl-build"
    glibc_build.mkdir()
    musl_build.mkdir()

    glibc_pkg = make_package(
        ocx, unique_repo, "1.0.0", glibc_build,
        platform="linux/amd64+libc.glibc", new=True, cascade=False,
    )
    musl_pkg = make_package(
        ocx, unique_repo, "1.0.0", musl_build,
        platform="linux/amd64+libc.musl", new=False, cascade=False,
    )
    assert glibc_pkg.marker != musl_pkg.marker, "markers must differ to discriminate entries"

    short = f"{unique_repo}:1.0.0"

    # Install with glibc seam — must pick the glibc entry.
    ocx.env["__OCX_TEST_LIBC"] = "glibc"
    try:
        result = ocx.json("package", "install", short)
    finally:
        ocx.env.pop("__OCX_TEST_LIBC", None)

    assert short in result, f"Install result missing key for {short}: {result}"
    installed_path = Path(result[short]["path"])
    assert installed_path.exists(), f"Installed package path does not exist: {installed_path}"

    # Verify candidate symlink was created.
    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )
    assert_symlink_exists(candidate)

    # Discriminating check: installed binary must be the glibc-marked one.
    installed_marker = _installed_marker_via_seam(ocx, short, "glibc")
    assert glibc_pkg.marker in installed_marker, (
        f"glibc host must install the glibc-marked binary; "
        f"expected marker {glibc_pkg.marker!r} but got: {installed_marker!r}"
    )
    assert musl_pkg.marker not in installed_marker, (
        f"glibc host must NOT install the musl-marked binary; "
        f"got musl marker in: {installed_marker!r}"
    )


@pytest.mark.skipif(
    sys.platform != "linux",
    reason="libc differentiation only applies to linux/amd64 hosts",
)
def test_install_selects_libc_musl_on_alpine_gcompat_host(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """ocx install on an Alpine+gcompat host selects the libc.musl entry, not glibc.

    Alpine+gcompat: the ld.so identity is the musl linker, so detection returns
    Musl even though gcompat could theoretically run glibc binaries.  OCX follows
    the "identity, not equivalence" rule from the ADR.

    Pushes both libc variants and asserts the musl marker is materialised when
    ``__OCX_TEST_LIBC=musl``, proving the seam drives real entry discrimination.
    """
    import platform as _platform
    if _platform.machine().lower() not in {"x86_64", "amd64"}:
        pytest.skip("test fixtures target linux/amd64")

    glibc_build = tmp_path / "glibc-build"
    musl_build = tmp_path / "musl-build"
    glibc_build.mkdir()
    musl_build.mkdir()

    glibc_pkg = make_package(
        ocx, unique_repo, "1.0.0", glibc_build,
        platform="linux/amd64+libc.glibc", new=True, cascade=False,
    )
    musl_pkg = make_package(
        ocx, unique_repo, "1.0.0", musl_build,
        platform="linux/amd64+libc.musl", new=False, cascade=False,
    )
    assert glibc_pkg.marker != musl_pkg.marker, "markers must differ to discriminate entries"

    short = f"{unique_repo}:1.0.0"

    # Install with musl seam — must pick the musl entry.
    ocx.env["__OCX_TEST_LIBC"] = "musl"
    try:
        result = ocx.json("package", "install", short)
    finally:
        ocx.env.pop("__OCX_TEST_LIBC", None)

    assert short in result, f"Install result missing key for {short}: {result}"
    installed_path = Path(result[short]["path"])
    assert installed_path.exists(), f"Installed package path does not exist: {installed_path}"

    # Candidate symlink must exist.
    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )
    assert_symlink_exists(candidate)

    # Discriminating check: installed binary must be the musl-marked one.
    installed_marker = _installed_marker_via_seam(ocx, short, "musl")
    assert musl_pkg.marker in installed_marker, (
        f"musl host must install the musl-marked binary; "
        f"expected marker {musl_pkg.marker!r} but got: {installed_marker!r}"
    )
    assert glibc_pkg.marker not in installed_marker, (
        f"musl host must NOT install the glibc-marked binary; "
        f"got glibc marker in: {installed_marker!r}"
    )


@pytest.mark.skipif(
    sys.platform != "linux",
    reason="libc differentiation only applies to linux/amd64 hosts",
)
def test_install_falls_back_when_libc_undetectable(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Install falls back to the untagged (empty os.features) entry when libc is undetectable.

    An undetectable libc (NixOS, minimal container) must not hard-fail — it must
    pick the legacy untagged linux/amd64 entry.  libc-tagged entries are
    unreachable because the host's detected os_features is None, which cannot
    satisfy any non-empty candidate os_features requirement.

    Pushes both a libc-tagged entry (glibc) and an untagged entry.  Installs
    with ``__OCX_TEST_LIBC=none`` and asserts the UNTAGGED marker is installed
    — not the glibc-tagged one — proving fallback is active and not a no-op.
    """
    import platform as _platform
    if _platform.machine().lower() not in {"x86_64", "amd64"}:
        pytest.skip("test fixtures target linux/amd64")

    glibc_build = tmp_path / "glibc-build"
    untagged_build = tmp_path / "untagged-build"
    glibc_build.mkdir()
    untagged_build.mkdir()

    glibc_pkg = make_package(
        ocx, unique_repo, "1.0.0", glibc_build,
        platform="linux/amd64+libc.glibc", new=True, cascade=False,
    )
    # Untagged entry — empty os.features, accepted by a host with no detected libc.
    untagged_pkg = make_package(
        ocx, unique_repo, "1.0.0", untagged_build,
        platform="linux/amd64", new=False, cascade=False,
    )
    assert glibc_pkg.marker != untagged_pkg.marker, "markers must differ to discriminate entries"

    short = f"{unique_repo}:1.0.0"

    # Simulate undetectable libc host.
    ocx.env["__OCX_TEST_LIBC"] = "none"
    try:
        result = ocx.json("package", "install", short)
    finally:
        ocx.env.pop("__OCX_TEST_LIBC", None)

    assert short in result, f"Install result missing key for {short}: {result}"
    installed_path = Path(result[short]["path"])
    assert installed_path.exists(), f"Installed package path does not exist: {installed_path}"

    # Candidate symlink must exist.
    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )
    assert_symlink_exists(candidate)

    # Discriminating check: installed binary must be the untagged fallback, not glibc.
    installed_marker = _installed_marker_via_seam(ocx, short, "none")
    assert untagged_pkg.marker in installed_marker, (
        f"undetectable-libc host must fall back to the untagged entry; "
        f"expected marker {untagged_pkg.marker!r} but got: {installed_marker!r}"
    )
    assert glibc_pkg.marker not in installed_marker, (
        f"undetectable-libc host must NOT install the glibc-tagged entry; "
        f"got glibc marker in: {installed_marker!r}"
    )


@pytest.mark.skipif(
    sys.platform != "linux",
    reason="libc differentiation only applies to linux/amd64 hosts",
)
def test_install_errors_when_no_compatible_entry(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Install fails with a clear ``feature mismatch:`` error when only a
    different-libc entry exists for the host's os+arch.

    Scenario: registry has ONLY a ``linux/amd64+libc.musl`` entry; the host
    declares glibc via ``__OCX_TEST_LIBC=glibc``. The musl entry shares the
    host os+arch but its ``os.features`` is not a subset of the glibc host's,
    so ``Index::select`` returns ``FeatureMismatch`` and install surfaces the
    ``feature mismatch:`` error.

    Ref: plan Directive 3 (FeatureMismatch rename), ADR
    §PackageErrorKind::FeatureMismatch.
    """
    # Publish a single linux/amd64 entry that requires libc.musl.
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        platform="linux/amd64+libc.musl", new=True, cascade=False,
    )

    # Host declares glibc — the musl-only entry cannot satisfy it.
    ocx.env["__OCX_TEST_LIBC"] = "glibc"
    try:
        proc = ocx.run(
            "package", "install", f"{unique_repo}:1.0.0",
            format="json",
            check=False,
        )
    finally:
        ocx.env.pop("__OCX_TEST_LIBC", None)

    assert proc.returncode == 65, (
        f"install with no compatible libc entry must exit 65 (DataError); got returncode={proc.returncode}"
    )
    assert "feature mismatch:" in proc.stderr, (
        f"expected 'feature mismatch:' in stderr; got: {proc.stderr}"
    )


@pytest.mark.skipif(
    sys.platform != "linux",
    reason="libc differentiation only applies to linux/amd64 hosts",
)
def test_install_errors_ambiguous_when_host_reports_both_libcs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Install fails with an ``ambiguous selection:`` error when the host
    advertises BOTH libc families and the index carries one entry per family.

    A dual-libc host (Ubuntu + musl-tools, or a multi-target CI runner) sets
    ``__OCX_TEST_LIBC=glibc,musl``, so `Platform::current()` reports
    ``os.features=["libc.glibc","libc.musl"]``. Both the ``libc.glibc`` and
    ``libc.musl`` index entries are then equally specific matches (each
    satisfies exactly one of the host's two features) — `Index::select`
    surfaces `SelectResult::Ambiguous`, and install exits 65 (DataError) with
    an ``ambiguous selection:`` message rather than silently picking one.
    """
    import platform as _platform
    if _platform.machine().lower() not in {"x86_64", "amd64"}:
        pytest.skip("test fixtures target linux/amd64")

    glibc_build = tmp_path / "glibc-build"
    musl_build = tmp_path / "musl-build"
    glibc_build.mkdir()
    musl_build.mkdir()

    # Two libc-marked entries under the SAME tag — pushes merge into one
    # image index (`new=True` then `new=False`, `cascade=False` keeps it
    # minimal), same fixture shape as
    # `test_install_discriminates_glibc_vs_musl_by_explicit_platform`.
    make_package(
        ocx, unique_repo, "1.0.0", glibc_build,
        platform="linux/amd64+libc.glibc", new=True, cascade=False,
    )
    make_package(
        ocx, unique_repo, "1.0.0", musl_build,
        platform="linux/amd64+libc.musl", new=False, cascade=False,
    )

    # Dual-libc host — the seam's comma-separated form yields {Glibc, Musl}.
    ocx.env["__OCX_TEST_LIBC"] = "glibc,musl"
    try:
        proc = ocx.run(
            "package", "install", f"{unique_repo}:1.0.0",
            format="json",
            check=False,
        )
    finally:
        ocx.env.pop("__OCX_TEST_LIBC", None)

    assert proc.returncode == 65, (
        f"install ambiguous between two equally-specific libc entries must exit 65 (DataError); "
        f"got returncode={proc.returncode}, stderr={proc.stderr!r}"
    )
    assert "ambiguous selection:" in proc.stderr, (
        f"expected 'ambiguous selection:' in stderr; got: {proc.stderr}"
    )


@pytest.mark.skipif(
    sys.platform != "linux",
    reason="libc differentiation only applies to linux/amd64 hosts",
)
def test_about_plain_does_not_double_render_libc(ocx: OcxRunner) -> None:
    """``ocx about`` (plain, piped) renders the bare os/arch on the Platforms
    row and the detected libc only on the dedicated Libc row.

    Regression test: `Platform::Display` gained a `+os_features` suffix, and
    the Platforms row previously rendered via `Display`, duplicating the
    libc onto both rows (e.g. `Platforms: linux/amd64+libc.glibc, any`).
    """
    ocx.env["__OCX_TEST_LIBC"] = "glibc"
    try:
        result = ocx.run("about", format=None)
    finally:
        ocx.env.pop("__OCX_TEST_LIBC", None)

    assert "Platforms: linux/amd64" in result.stdout, (
        f"Platforms row must render the bare os/arch, no +os_features suffix; got: {result.stdout!r}"
    )
    assert result.stdout.count("libc.glibc") == 1, (
        f"libc.glibc must appear exactly once (the dedicated Libc row), not duplicated onto Platforms; "
        f"got: {result.stdout!r}"
    )


@pytest.mark.skipif(
    sys.platform != "linux",
    reason="libc differentiation only applies to linux/amd64 hosts",
)
def test_version_verbose_does_not_double_render_libc(ocx: OcxRunner) -> None:
    """``ocx version -v`` renders the detected libc once, in the host-row
    parenthetical, not duplicated by the bare os/arch base.

    Regression test: `Platform::Display` gained a `+os_features` suffix. The
    verbose `host:` row renders `platform.segments().join("/")` (bare os/arch)
    and shows the detected libc separately in the parenthetical, so a revert to
    `platform.to_string()` would duplicate `libc.glibc` (`linux/amd64+libc.glibc
    (libc.glibc)`). Sibling of `test_about_plain_does_not_double_render_libc`.
    """
    ocx.env["__OCX_TEST_LIBC"] = "glibc"
    try:
        result = ocx.run("version", "-v", format=None)
    finally:
        ocx.env.pop("__OCX_TEST_LIBC", None)

    assert "linux/amd64 (libc.glibc)" in result.stdout, (
        f"host row must render the bare os/arch plus the libc parenthetical; got: {result.stdout!r}"
    )
    assert result.stdout.count("libc.glibc") == 1, (
        f"libc.glibc must appear exactly once (the host-row parenthetical), not duplicated onto the "
        f"os/arch base; got: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# W6 — end-to-end libc discrimination via explicit --platform os.features
# ---------------------------------------------------------------------------
#
# Unblocked by the custom `--platform os/arch[+feature...]` syntax (Directive 4):
# `ocx package push -p linux/amd64+libc.glibc` publishes an index entry whose
# `os.features` is `["libc.glibc"]`, and `ocx package install --platform
# linux/amd64+libc.glibc` selects exactly that entry. We publish both a glibc-
# and a musl-marked entry under ONE tag (two pushes to the same tag merge into
# one image index via the cascade/merge path), then install each explicitly and
# assert the matching marker binary is materialised.


def _package_root(ocx: OcxRunner, short: str, platform: str) -> Path:
    """Return the installed package root for ``short`` at ``platform``.

    ``package which`` re-runs platform selection, so the explicit
    ``--platform`` (carrying the os.features) must be passed to resolve the
    same entry that was installed — the real host probe would otherwise reject
    a non-host-libc entry.
    """
    which = ocx.json("package", "which", "--platform", platform, short)
    return Path(which[short])


def _installed_marker(ocx: OcxRunner, short: str, platform: str) -> str:
    """Read the marker echoed by the installed `bin/hello` script.

    The content tree (including `bin/hello`) lives under ``<root>/content/``.
    ``bin/hello`` contains ``echo <marker>``, so the marker uniquely
    identifies which pushed entry is currently installed.
    """
    hello = _package_root(ocx, short, platform) / "content" / "bin" / "hello"
    return hello.read_text()


@pytest.mark.skipif(
    sys.platform != "linux",
    reason="libc differentiation only applies to linux/amd64 hosts",
)
def test_install_discriminates_glibc_vs_musl_by_explicit_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Two libc-marked entries under one tag; --platform selects the right one.

    Pushes a `linux/amd64+libc.glibc` entry and a `linux/amd64+libc.musl` entry
    under the same `repo:1.0.0` tag, each with a distinct marker binary. Then:
      - install --platform linux/amd64+libc.glibc → glibc marker binary
      - install --platform linux/amd64+libc.musl  → musl marker binary
    """
    import platform as _platform

    if _platform.machine().lower() not in {"x86_64", "amd64"}:
        pytest.skip("test fixtures target linux/amd64")

    # Distinct build dirs per push — `make_package` derives its content dir from
    # repo+tag, which are identical here (both publish `repo:1.0.0`).
    glibc_build = tmp_path / "glibc-build"
    musl_build = tmp_path / "musl-build"
    glibc_build.mkdir()
    musl_build.mkdir()

    # First entry: glibc-marked. `new=True` creates the tag; `cascade=False`
    # keeps the index minimal so the two pushes merge into one image index.
    glibc_pkg = make_package(
        ocx, unique_repo, "1.0.0", glibc_build,
        platform="linux/amd64+libc.glibc", new=True, cascade=False,
    )
    # Second entry: musl-marked under the SAME tag. `new=False` merges into the
    # existing image index instead of replacing it.
    musl_pkg = make_package(
        ocx, unique_repo, "1.0.0", musl_build,
        platform="linux/amd64+libc.musl", new=False, cascade=False,
    )
    assert glibc_pkg.marker != musl_pkg.marker, "markers must differ to discriminate entries"

    short = f"{unique_repo}:1.0.0"

    # Install the glibc variant explicitly.
    ocx.json("package", "install", "--platform", "linux/amd64+libc.glibc", short)
    assert glibc_pkg.marker in _installed_marker(ocx, short, "linux/amd64+libc.glibc"), (
        "explicit --platform linux/amd64+libc.glibc must install the glibc-marked binary"
    )
    glibc_root = _package_root(ocx, short, "linux/amd64+libc.glibc")

    # Uninstall + reinstall the musl variant explicitly. Uninstall clears the
    # candidate symlink so the next install re-materialises the musl entry; the
    # musl content is a distinct digest, so its package root differs.
    ocx.run("package", "uninstall", short, format="json", check=False)
    ocx.json("package", "install", "--platform", "linux/amd64+libc.musl", short)
    assert musl_pkg.marker in _installed_marker(ocx, short, "linux/amd64+libc.musl"), (
        "explicit --platform linux/amd64+libc.musl must install the musl-marked binary"
    )
    musl_root = _package_root(ocx, short, "linux/amd64+libc.musl")
    assert glibc_root != musl_root, "glibc and musl entries must resolve to distinct content"


# ---------------------------------------------------------------------------
# Real-host detection markers (skipped unless OCX_REAL_HOST_LIBC_TESTS is set)
# ---------------------------------------------------------------------------


@requires_real_host
def test_detect_reports_glibc_on_nixos_store_interp(ocx: OcxRunner) -> None:
    """NixOS without nix-ld: the loader lives under ``/nix/store``, not an FHS
    path. PT_INTERP discovery reads it from a system binary, so ``ocx about``
    reports ``libc.glibc`` where the FHS-only allowlist would have detected
    nothing.

    Manual: run with ``OCX_REAL_HOST_LIBC_TESTS=1`` on a stock NixOS host.
    """
    about = ocx.json("--offline", "about")
    assert "libc.glibc" in about.get("libc", []), (
        f"PT_INTERP /nix/store discovery must detect glibc; got libc={about.get('libc')}"
    )


@requires_real_host
def test_detect_reports_libc_on_gentoo_prefix(ocx: OcxRunner) -> None:
    """Gentoo Prefix: the loader lives under the prefix root, not an FHS path.
    PT_INTERP discovery must still find it, so ``ocx about`` reports a non-empty
    ``libc`` set.

    Manual: run with ``OCX_REAL_HOST_LIBC_TESTS=1`` inside a Gentoo Prefix.
    """
    about = ocx.json("--offline", "about")
    assert about.get("libc"), (
        f"PT_INTERP discovery must detect a libc family on a Gentoo Prefix host; "
        f"got libc={about.get('libc')}"
    )
