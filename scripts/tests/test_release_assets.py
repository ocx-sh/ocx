"""`release_assets.py`'s proofs: the asset set equals v0.6.6's, and each archive keeps its layout."""

from __future__ import annotations

import json
import shutil
import subprocess
import tarfile
import zipfile
from pathlib import Path

import pytest
import release_assets as ra
from cdx_stub import run_cyclonedx

FIXTURES = Path(__file__).parent / "fixtures" / "v0.6.6"
CDX_JSON = {f"ocx-{t}.cdx.json" for t in ra.TARGETS}


def as_v066(text: str) -> str:
    """v0.6.6 shipped the Windows archives as `msvc`; every comparison with its fixture goes through here."""
    return text.replace("pc-windows-gnullvm", "pc-windows-msvc")


@pytest.fixture
def built(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    # The real `cargo_cyclonedx` runs; only the external tool is replaced, by an honest stub.
    monkeypatch.setattr(ra.subprocess, "run", lambda argv, **_: run_cyclonedx(argv))
    root, bins = tmp_path / "root", tmp_path / "bins"
    root.mkdir()
    for name in ra.EXTRA_FILES:
        (root / name).write_text(name, encoding="utf-8")
    for triple in ra.TARGETS:
        exe = bins / triple / ("ocx.exe" if "windows" in triple else "ocx")
        exe.parent.mkdir(parents=True)
        exe.write_bytes(b"binary " + triple.encode())
    dist = tmp_path / "dist"
    ra.build(dist, "0.6.6", "a" * 40, bins, root)
    return dist


def v066_names() -> set[str]:
    return set((FIXTURES / "assets.txt").read_text(encoding="utf-8").split())


def test_asset_names_equal_v066_modulo_the_windows_name_plus_the_per_platform_sbom(built: Path) -> None:
    produced = {as_v066(p.name) for p in built.iterdir()}
    expected = v066_names()
    assert len(expected) == 19, "the v0.6.6 fixture stopped being read"
    cdx = {as_v066(n) for n in CDX_JSON}
    assert produced - cdx == expected, (
        f"missing {expected - produced}, extra {produced - cdx - expected}"
    )
    assert cdx <= produced


def test_windows_archives_are_named_for_the_gnullvm_target_they_contain() -> None:
    assert {t for t in ra.TARGETS if "windows" in t} == {
        "x86_64-pc-windows-gnullvm",
        "aarch64-pc-windows-gnullvm",
    }
    assert not any("msvc" in t for t in ra.TARGETS)


def test_the_unix_archive_layout_matches_v066(built: Path) -> None:
    listing = json.loads(
        (FIXTURES / "archive-listing.json").read_text(encoding="utf-8")
    )
    name = "ocx-x86_64-unknown-linux-gnu.tar.gz"
    with tarfile.open(built / name) as tar:
        assert sorted(tar.getnames()) == sorted(n.rstrip("/") for n in listing[name])
        assert tar.getmember("ocx-x86_64-unknown-linux-gnu/ocx").mode == 0o755


def test_the_windows_archive_layout_matches_v066_modulo_the_windows_name(built: Path) -> None:
    listing = json.loads(
        (FIXTURES / "archive-listing.json").read_text(encoding="utf-8")
    )
    name = "ocx-x86_64-pc-windows-gnullvm.zip"
    with zipfile.ZipFile(built / name) as zf:
        assert sorted(zf.namelist()) == sorted(listing[as_v066(name)])


def test_checksums_follow_the_v066_format(built: Path) -> None:
    fixture = (FIXTURES / "sha256.sum").read_text(encoding="utf-8")
    ours = (built / "sha256.sum").read_text(encoding="utf-8")
    assert [as_v066(line.split("*")[1]) for line in ours.splitlines() if line] == [
        line.split("*")[1] for line in fixture.splitlines() if line
    ]
    assert ours.endswith("\n\n") and fixture.endswith("\n\n")
    per = (built / "ocx-x86_64-pc-windows-gnullvm.zip.sha256").read_text(encoding="utf-8")
    assert per.endswith(" *ocx-x86_64-pc-windows-gnullvm.zip\n\n")


def test_the_manifest_carries_the_v066_artifact_fields(built: Path) -> None:
    """Compares `artifacts` and `releases[0]` only.

    The generator drops v0.6.6's other top-level fields (`dist_version`, `announcement_title`,
    `systems`, ...). No known consumer reads them: setup-ocx `download.ts` and www-setup `gen-dist.sh`
    read the `.sha256` files and `sha256.sum` only.
    """
    fixture = json.loads((FIXTURES / "dist-manifest.json").read_text(encoding="utf-8"))
    ours = json.loads(as_v066((built / "dist-manifest.json").read_text(encoding="utf-8")))
    assert set(ours["artifacts"]) == set(fixture["artifacts"])
    for name, art in fixture["artifacts"].items():
        assert set(ours["artifacts"][name]) == set(art), name
        if "assets" in art:
            assert ours["artifacts"][name]["assets"] == art["assets"], name
    assert ours["releases"][0]["app_name"] == "ocx" and ours["releases"][0]["artifacts"]


def test_a_missing_binary_is_refused(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        ra.build(
            tmp_path / "d", "1.0.0", "a" * 40, tmp_path / "none", tmp_path
        )


def test_the_real_pinned_tool_writes_the_file_cargo_cyclonedx_returns(tmp_path: Path) -> None:
    """Drives `ocx exec -g docs -- cargo cyclonedx` for real; skips in lanes without `ocx` or the tool.

    Sandboxed lanes (no `ocx` on PATH, no network to fetch the tool) skip it.
    """
    probe = subprocess.run(
        ["ocx", "exec", "-g", "docs", "--", "cargo", "cyclonedx", "--version"],
        capture_output=True,
        check=False,
        cwd=Path(__file__).resolve().parents[2],
    ) if shutil.which("ocx") else None
    if probe is None or probe.returncode != 0:
        pytest.skip("probed `ocx exec -g docs -- cargo cyclonedx --version`: `ocx` or the pinned tool is unavailable")
    crate = tmp_path / "crates" / "ocx_cli"
    (crate / "src").mkdir(parents=True)
    (crate / "Cargo.toml").write_text('[package]\nname = "tiny"\nversion = "0.1.0"\nedition = "2021"\n', encoding="utf-8")
    (crate / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    real = subprocess.run

    def via_ocx(argv: list[str], **kw: object) -> subprocess.CompletedProcess[bytes]:
        # The repo root carries the `ocx.toml` that pins the tool; the manifest path is absolute.
        kw["cwd"] = Path(__file__).resolve().parents[2]
        return real(["ocx", "exec", "-g", "docs", "--", *argv], **kw)  # type: ignore[arg-type]

    with pytest.MonkeyPatch.context() as mp:
        mp.setattr(ra.subprocess, "run", via_ocx)
        for fmt in ("json", "xml"):
            out = ra.cargo_cyclonedx(tmp_path, fmt, None, "ocx-x")
            assert out.is_file(), f"cargo_cyclonedx returned {out} but the tool did not write it"
