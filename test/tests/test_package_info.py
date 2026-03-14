"""Tests for `ocx package info` — reading description metadata."""

import json
from pathlib import Path
from uuid import uuid4

from src import OcxRunner, current_platform
from src.helpers import make_package


def _push_description(
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    *,
    title: str = "Test Tool",
    description: str = "A test tool for testing",
    keywords: str = "test,tool",
    readme_text: str = "# Hello\n\nThis is a test README.\n",
    logo: bool = False,
) -> None:
    """Push a description to the given repo."""
    readme_path = tmp_path / "README.md"
    readme_path.write_text(readme_text)
    fq = f"{ocx.registry}/{repo}:1.0.0"

    args = [
        "package", "describe",
        "--readme", str(readme_path),
        "--title", title,
        "--description", description,
        "--keywords", keywords,
    ]

    if logo:
        # Create a tiny 1x1 PNG
        logo_path = tmp_path / "logo.png"
        # Minimal valid PNG (1x1 transparent pixel)
        logo_path.write_bytes(
            b"\x89PNG\r\n\x1a\n"
            b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01"
            b"\x08\x06\x00\x00\x00\x1f\x15\xc4\x89"
            b"\x00\x00\x00\nIDATx\x9cc\x00\x01\x00\x00\x05\x00\x01"
            b"\r\n\xb4\x00\x00\x00\x00IEND\xaeB`\x82"
        )
        args += ["--logo", str(logo_path)]

    args.append(fq)
    ocx.plain(*args)


def test_info_no_description(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """package info on a repo without __ocx.desc returns 'No description found'."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    result = ocx.plain("package", "info", pkg.fq)
    assert "No description found" in result.stdout


def test_info_with_description(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """package info returns title, description, keywords after describe push."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    _push_description(ocx, unique_repo, tmp_path, title="My Tool", description="A great tool", keywords="my,tool")

    result = ocx.plain("package", "info", pkg.fq)
    assert "Title:       My Tool" in result.stdout
    assert "Description: A great tool" in result.stdout
    assert "Keywords:    my,tool" in result.stdout


def test_info_json(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """package info --format json returns structured object."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    _push_description(ocx, unique_repo, tmp_path, title="CMake", description="Build system", keywords="cmake,build")

    data = ocx.json("package", "info", pkg.fq)
    assert data["title"] == "CMake"
    assert data["description"] == "Build system"
    assert data["keywords"] == "cmake,build"


def test_info_json_no_description(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """package info --format json returns null when no description exists."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    data = ocx.json("package", "info", pkg.fq)
    assert data is None


def test_info_save_readme(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """--save-readme writes the README content to a file."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    readme_content = "# My Tool\n\nThis is the readme.\n"
    _push_description(ocx, unique_repo, tmp_path, readme_text=readme_content)

    save_path = tmp_path / "output" / "readme.md"
    ocx.plain("package", "info", "--save-readme", str(save_path), pkg.fq)
    assert save_path.exists()
    assert save_path.read_text() == readme_content


def test_info_save_readme_to_dir(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """--save-readme with a directory path uses README.md as default filename."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    readme_content = "# Dir Test\n"
    _push_description(ocx, unique_repo, tmp_path, readme_text=readme_content)

    out_dir = tmp_path / "outdir"
    out_dir.mkdir()
    ocx.plain("package", "info", "--save-readme", str(out_dir), pkg.fq)
    assert (out_dir / "README.md").exists()
    assert (out_dir / "README.md").read_text() == readme_content


def test_info_save_logo(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """--save-logo writes the logo file."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    _push_description(ocx, unique_repo, tmp_path, logo=True)

    save_path = tmp_path / "output" / "my-logo.png"
    ocx.plain("package", "info", "--save-logo", str(save_path), pkg.fq)
    assert save_path.exists()
    # Should start with PNG magic bytes
    data = save_path.read_bytes()
    assert data[:4] == b"\x89PNG"
