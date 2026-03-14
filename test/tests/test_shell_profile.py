import json
from pathlib import Path

from src import OcxRunner, PackageInfo, make_package


# ---------------------------------------------------------------------------
# Add — basic modes
# ---------------------------------------------------------------------------


def test_profile_add_default_is_candidate(ocx: OcxRunner, published_package: PackageInfo):
    """Default mode (no flag) is candidate"""
    pkg = published_package
    ocx.json("install", pkg.short)

    result = ocx.json("shell", "profile", "add", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "added"
    assert result[0]["mode"] == "candidate"


def test_profile_add_candidate(ocx: OcxRunner, published_package: PackageInfo):
    """ocx install <pkg> && ocx shell profile add --candidate <pkg>"""
    pkg = published_package
    ocx.json("install", pkg.short)

    result = ocx.json("shell", "profile", "add", "--candidate", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "added"
    assert result[0]["mode"] == "candidate"


def test_profile_add_current(ocx: OcxRunner, published_package: PackageInfo):
    """ocx install -s <pkg> && ocx shell profile add --current <pkg>"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)

    result = ocx.json("shell", "profile", "add", "--current", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "added"
    assert result[0]["mode"] == "current"


def test_profile_add_content(ocx: OcxRunner, published_package: PackageInfo):
    """ocx install <pkg> && ocx shell profile add --content <pkg>"""
    pkg = published_package
    ocx.json("install", pkg.short)

    result = ocx.json("shell", "profile", "add", "--content", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "added"
    assert result[0]["mode"] == "content"

    # Verify profile manifest includes content_digest
    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())
    assert profile["packages"][0]["mode"] == "content"
    assert "content_digest" in profile["packages"][0]
    assert profile["packages"][0]["content_digest"].startswith("sha256:")


# ---------------------------------------------------------------------------
# Add — auto-install
# ---------------------------------------------------------------------------


def test_profile_add_auto_installs(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile add <pkg> without prior install auto-installs"""
    pkg = published_package
    # Don't install — profile add should auto-install
    result = ocx.json("shell", "profile", "add", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "added"
    assert result[0]["mode"] == "candidate"

    # Verify the package is now installed
    find_result = ocx.json("find", pkg.short)
    assert pkg.short in find_result


def test_profile_add_auto_installs_content(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile add --content <pkg> without install auto-installs"""
    pkg = published_package
    result = ocx.json("shell", "profile", "add", "--content", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "added"
    assert result[0]["mode"] == "content"


def test_profile_add_auto_installs_candidate(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile add --candidate <pkg> without install auto-installs"""
    pkg = published_package
    result = ocx.json("shell", "profile", "add", "--candidate", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "added"
    assert result[0]["mode"] == "candidate"


def test_profile_add_bare_name_as_latest(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Bare name (no tag) in candidate mode resolves to candidates/latest"""
    pkg = published_package
    ocx.json("install", pkg.short)

    result = ocx.json("shell", "profile", "add", pkg.repo)

    assert len(result) == 1
    assert result[0]["status"] == "added"
    assert result[0]["mode"] == "candidate"


def test_profile_add_auto_installs_offline_fails(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Offline + uninstalled = error"""
    pkg = published_package
    result = ocx.run(
        "--offline", "shell", "profile", "add", pkg.short, check=False
    )
    assert result.returncode != 0


# ---------------------------------------------------------------------------
# Add — error paths
# ---------------------------------------------------------------------------


def test_profile_add_nonexistent_fails(ocx: OcxRunner):
    """ocx shell profile add <nonexistent> should fail gracefully"""
    result = ocx.run(
        "shell", "profile", "add", "nonexistent/pkg:1.0.0", check=False
    )
    assert result.returncode != 0


# ---------------------------------------------------------------------------
# Add — multi-package
# ---------------------------------------------------------------------------


def test_profile_add_multiple_packages(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Adding multiple packages in one command"""
    pkg1 = make_package(ocx, f"{unique_repo}_a", "1.0.0", tmp_path, new=True)
    pkg2 = make_package(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, new=True)
    ocx.json("install", pkg1.short)
    ocx.json("install", pkg2.short)

    result = ocx.json("shell", "profile", "add", pkg1.short, pkg2.short)

    assert len(result) == 2
    assert result[0]["status"] == "added"
    assert result[1]["status"] == "added"

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())
    assert len(profile["packages"]) == 2


# ---------------------------------------------------------------------------
# Add — idempotency and mode switching
# ---------------------------------------------------------------------------


def test_profile_add_duplicate_is_idempotent(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Adding the same package twice should not create duplicates"""
    pkg = published_package
    ocx.json("install", pkg.short)

    result1 = ocx.json("shell", "profile", "add", pkg.short)
    assert result1[0]["status"] == "added"

    result2 = ocx.json("shell", "profile", "add", pkg.short)
    assert result2[0]["status"] == "updated"

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())
    assert len(profile["packages"]) == 1


def test_profile_add_mode_current_to_candidate(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Re-adding a current-mode entry as candidate updates the mode"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)

    ocx.json("shell", "profile", "add", "--current", pkg.short)
    result = ocx.json("shell", "profile", "add", "--candidate", pkg.short)

    assert result[0]["status"] == "updated"
    assert result[0]["mode"] == "candidate"

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())
    assert len(profile["packages"]) == 1
    assert profile["packages"][0]["mode"] == "candidate"


def test_profile_add_mode_candidate_to_content(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Re-adding a candidate-mode entry as content updates the mode"""
    pkg = published_package
    ocx.json("install", pkg.short)

    ocx.json("shell", "profile", "add", "--candidate", pkg.short)
    result = ocx.json("shell", "profile", "add", "--content", pkg.short)

    assert result[0]["status"] == "updated"
    assert result[0]["mode"] == "content"

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())
    assert len(profile["packages"]) == 1
    assert profile["packages"][0]["mode"] == "content"


def test_profile_add_mode_content_to_current(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Re-adding a content-mode entry as current updates the mode"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)

    ocx.json("shell", "profile", "add", "--content", pkg.short)
    result = ocx.json("shell", "profile", "add", "--current", pkg.short)

    assert result[0]["status"] == "updated"
    assert result[0]["mode"] == "current"

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())
    assert len(profile["packages"]) == 1
    assert profile["packages"][0]["mode"] == "current"


def test_profile_add_mode_switch_warns(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Mode switch emits stderr warning"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)

    ocx.json("shell", "profile", "add", "--current", pkg.short)
    result = ocx.run(
        "shell", "profile", "add", "--candidate", pkg.short, check=True
    )
    assert "mode changed" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Add — same-repo warnings
# ---------------------------------------------------------------------------


def test_profile_add_same_repo_warns(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """Adding a second version of the same repo emits stderr warning"""
    v1, v2 = published_two_versions
    ocx.json("install", v1.short)
    ocx.json("install", v2.short)

    ocx.json("shell", "profile", "add", "--candidate", v1.short)
    result = ocx.run(
        "shell", "profile", "add", "--candidate", v2.short, check=True
    )
    assert "already in your shell profile" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Add — two-version scenarios
# ---------------------------------------------------------------------------


def test_profile_add_candidate_two_versions(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """Two versions of the same repo can both be profiled as candidates"""
    v1, v2 = published_two_versions
    ocx.json("install", v1.short)
    ocx.json("install", v2.short)

    r1 = ocx.json("shell", "profile", "add", "--candidate", v1.short)
    r2 = ocx.json("shell", "profile", "add", "--candidate", v2.short)

    assert r1[0]["status"] == "added"
    assert r2[0]["status"] == "added"

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())
    # Different tags = different identifiers = two entries
    assert len(profile["packages"]) == 2


# ---------------------------------------------------------------------------
# Remove
# ---------------------------------------------------------------------------


def test_profile_remove(ocx: OcxRunner, published_package: PackageInfo):
    """ocx shell profile add <pkg> && ocx shell profile remove <pkg>"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.json("shell", "profile", "remove", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "removed"


def test_profile_remove_absent(ocx: OcxRunner, published_package: PackageInfo):
    """Removing a package not in the profile reports absent"""
    pkg = published_package

    result = ocx.json("shell", "profile", "remove", pkg.short)

    assert len(result) == 1
    assert result[0]["status"] == "absent"


def test_profile_remove_multiple_packages(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Removing multiple packages in one command"""
    pkg1 = make_package(ocx, f"{unique_repo}_a", "1.0.0", tmp_path, new=True)
    pkg2 = make_package(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, new=True)
    ocx.json("install", pkg1.short)
    ocx.json("install", pkg2.short)
    ocx.json("shell", "profile", "add", pkg1.short, pkg2.short)

    result = ocx.json("shell", "profile", "remove", pkg1.short, pkg2.short)

    assert len(result) == 2
    assert result[0]["status"] == "removed"
    assert result[1]["status"] == "removed"

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())
    assert len(profile["packages"]) == 0


def test_profile_remove_keeps_install(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Removing from profile leaves the package installed"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    ocx.json("shell", "profile", "remove", pkg.short)

    # Package is still findable
    find_result = ocx.json("find", pkg.short)
    assert pkg.short in find_result


# ---------------------------------------------------------------------------
# List
# ---------------------------------------------------------------------------


def test_profile_list_active(ocx: OcxRunner, published_package: PackageInfo):
    """ocx shell profile list shows active status"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.json("shell", "profile", "list")
    assert len(result) == 1
    assert result[0]["status"] == "active"
    assert result[0]["mode"] == "candidate"


def test_profile_list_content_mode(ocx: OcxRunner, published_package: PackageInfo):
    """ocx shell profile list shows content mode and active status"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--content", pkg.short)

    result = ocx.json("shell", "profile", "list")
    assert len(result) == 1
    assert result[0]["status"] == "active"
    assert result[0]["mode"] == "content"


def test_profile_list_broken(ocx: OcxRunner, published_package: PackageInfo):
    """ocx shell profile list shows broken status after uninstall removes candidate"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    # Remove the candidate symlink
    ocx.json("uninstall", pkg.short)

    result = ocx.json("shell", "profile", "list")
    assert len(result) == 1
    assert result[0]["status"] == "broken"


def test_profile_list_broken_candidate(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile list shows broken after candidate symlink is removed"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--candidate", pkg.short)

    # Remove the candidate symlink
    ocx.json("uninstall", pkg.short)

    result = ocx.json("shell", "profile", "list")
    assert len(result) == 1
    assert result[0]["status"] == "broken"


def test_profile_list_broken_content(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile list shows broken after content object is purged"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--content", pkg.short)

    # Purge the object so the content path no longer exists
    ocx.json("uninstall", "--purge", pkg.short)

    result = ocx.json("shell", "profile", "list")
    assert len(result) == 1
    assert result[0]["status"] == "broken"


def test_profile_list_multiple_packages(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """ocx shell profile list shows multiple entries in order"""
    pkg1 = make_package(ocx, f"{unique_repo}_a", "1.0.0", tmp_path, new=True)
    pkg2 = make_package(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, new=True)
    ocx.json("install", pkg1.short)
    ocx.json("install", pkg2.short)
    ocx.json("shell", "profile", "add", pkg1.short, pkg2.short)

    result = ocx.json("shell", "profile", "list")

    assert len(result) == 2
    # Order matches insertion order
    packages = [e["package"] for e in result]
    assert pkg1.repo in packages[0]
    assert pkg2.repo in packages[1]


def test_profile_list_empty(ocx: OcxRunner):
    """Empty profile returns empty array, not error"""
    result = ocx.json("shell", "profile", "list")
    assert result == []


# ---------------------------------------------------------------------------
# List — JSON structure validation
# ---------------------------------------------------------------------------


def test_profile_add_json_structure(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Verify add JSON shape: array of {package, mode, status}"""
    pkg = published_package
    ocx.json("install", pkg.short)

    result = ocx.json("shell", "profile", "add", pkg.short)

    assert isinstance(result, list)
    entry = result[0]
    assert "package" in entry
    assert "mode" in entry
    assert "status" in entry
    assert entry["mode"] in ("current", "candidate", "content")
    assert entry["status"] in ("added", "updated")


def test_profile_remove_json_structure(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Verify remove JSON shape: array of {package, status}"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.json("shell", "profile", "remove", pkg.short)

    assert isinstance(result, list)
    entry = result[0]
    assert "package" in entry
    assert "status" in entry
    assert entry["status"] in ("removed", "absent")


def test_profile_list_json_structure(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Verify list JSON shape: array of {package, mode, status, path}"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.json("shell", "profile", "list")

    assert isinstance(result, list)
    entry = result[0]
    assert "package" in entry
    assert "mode" in entry
    assert "status" in entry
    assert "path" in entry
    assert entry["mode"] in ("current", "candidate", "content")
    assert entry["status"] in ("active", "broken")
    assert entry["path"] is not None


# ---------------------------------------------------------------------------
# Plain text output
# ---------------------------------------------------------------------------


def test_profile_list_plain_output(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Plain format outputs a readable table with headers"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run("shell", "profile", "list", format=None)

    assert "Package" in result.stdout
    assert "Mode" in result.stdout
    assert "Status" in result.stdout
    assert "active" in result.stdout


def test_profile_add_plain_output(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Plain format outputs a readable table with headers"""
    pkg = published_package
    ocx.json("install", pkg.short)

    result = ocx.run("shell", "profile", "add", pkg.short, format=None)

    assert "Package" in result.stdout
    assert "Mode" in result.stdout
    assert "Status" in result.stdout
    assert "added" in result.stdout


# ---------------------------------------------------------------------------
# Load
# ---------------------------------------------------------------------------


def test_profile_load_outputs_exports(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile load outputs shell export statements"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert "export" in result.stdout
    assert "PATH" in result.stdout


def test_profile_load_content_mode(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile load resolves content-mode entries via object store"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--content", pkg.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert "export" in result.stdout
    assert "PATH" in result.stdout
    # Content-mode paths go through objects/, not installs/
    assert "objects" in result.stdout


def test_profile_load_skips_broken(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile load skips broken entries with stderr warning"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    # Break the symlink by uninstalling (removes candidate)
    ocx.json("uninstall", pkg.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    # Should succeed with no output (broken entry skipped)
    assert result.returncode == 0
    assert result.stdout.strip() == ""


def test_profile_load_skips_broken_candidate(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile load skips broken candidate entries"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--candidate", pkg.short)

    # Break by uninstalling (removes candidate symlink)
    ocx.json("uninstall", pkg.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert result.returncode == 0
    assert result.stdout.strip() == ""


def test_profile_load_skips_broken_content(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile load skips broken content entries"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--content", pkg.short)

    # Break by purging (removes object store content)
    ocx.json("uninstall", "--purge", pkg.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert result.returncode == 0
    assert result.stdout.strip() == ""


def test_profile_load_warns_broken(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx shell profile load warns on stderr for broken entries"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    # Break the symlink by uninstalling (removes candidate)
    ocx.json("uninstall", pkg.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None,
        log_level="warn"
    )
    assert result.returncode == 0
    assert "skipping" in result.stderr.lower()


def test_profile_load_mixed_active_broken(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Load emits exports only for active entries, skips broken ones"""
    pkg1 = make_package(ocx, f"{unique_repo}_a", "1.0.0", tmp_path, new=True)
    pkg2 = make_package(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, new=True)
    ocx.json("install", pkg1.short)
    ocx.json("install", pkg2.short)
    ocx.json("shell", "profile", "add", pkg1.short, pkg2.short)

    # Break pkg1 by uninstalling (removes candidate)
    ocx.json("uninstall", pkg1.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert result.returncode == 0
    # pkg2 should still produce exports
    assert "export" in result.stdout
    assert "PATH" in result.stdout


def test_profile_load_multiple_packages(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Load emits exports for all profiled packages with accumulated env"""
    pkg1 = make_package(ocx, f"{unique_repo}_a", "1.0.0", tmp_path, new=True)
    pkg2 = make_package(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, new=True)
    ocx.json("install", pkg1.short)
    ocx.json("install", pkg2.short)
    ocx.json("shell", "profile", "add", pkg1.short, pkg2.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    # Both packages declare PATH and HELLO_HOME
    lines = result.stdout.strip().split("\n")
    path_lines = [l for l in lines if "PATH" in l and "export" in l]
    home_lines = [l for l in lines if "HELLO_HOME" in l and "export" in l]
    # Each package emits its own set of exports
    assert len(path_lines) >= 2
    assert len(home_lines) >= 2


def test_profile_load_candidate_correct_version(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """Candidate mode loads env from the specific tagged version"""
    v1, v2 = published_two_versions
    ocx.json("install", v1.short)
    ocx.json("install", v2.short)

    # Profile v1 as candidate
    ocx.json("shell", "profile", "add", "--candidate", v1.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert "export" in result.stdout
    # The path should contain the v1 tag in the candidate symlink path
    assert f"candidates/{v1.tag}" in result.stdout


def test_profile_load_follows_select(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """Current-mode entry follows select: install v1 --select, add --current, select v2"""
    v1, v2 = published_two_versions
    ocx.json("install", "-s", v1.short)
    ocx.json("install", v2.short)

    # Profile the repo (current mode, explicit flag)
    ocx.json("shell", "profile", "add", "--current", v1.short)

    # Now select v2
    ocx.json("select", v2.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert "export" in result.stdout
    # The current symlink now points to v2's content


def test_profile_load_empty_profile(ocx: OcxRunner):
    """No entries → no output, exit 0"""
    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert result.returncode == 0
    assert result.stdout.strip() == ""


def test_profile_load_no_profile_file(ocx: OcxRunner):
    """Missing profile.json → no output, exit 0"""
    # Verify profile.json doesn't exist yet
    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    assert not profile_path.exists()

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert result.returncode == 0
    assert result.stdout.strip() == ""


def test_profile_load_bash_format(
    ocx: OcxRunner, published_package: PackageInfo
):
    """--shell bash emits 'export KEY=value' syntax"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )
    for line in result.stdout.strip().split("\n"):
        if line.strip():
            assert line.strip().startswith("export "), f"unexpected line: {line}"


def test_profile_load_fish_format(
    ocx: OcxRunner, published_package: PackageInfo
):
    """--shell fish emits fish-style 'set -x' export syntax"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run(
        "shell", "profile", "load", "--shell", "fish", format=None
    )
    for line in result.stdout.strip().split("\n"):
        if line.strip():
            assert line.strip().startswith("set "), f"unexpected line: {line}"


def test_profile_load_offline(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Profile load works in offline mode (symlink modes need no network)"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run(
        "--offline", "shell", "profile", "load", "--shell", "bash", format=None
    )
    assert result.returncode == 0
    assert "export" in result.stdout


def test_profile_load_content_vs_symlink_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Content-mode path uses objects/, candidate-mode uses installs/"""
    pkg = published_package
    ocx.json("install", pkg.short)

    # Add as candidate mode (default), get load output
    ocx.json("shell", "profile", "add", pkg.short)
    result_candidate = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )

    # Switch to content mode
    ocx.json("shell", "profile", "add", "--content", pkg.short)
    result_content = ocx.run(
        "shell", "profile", "load", "--shell", "bash", format=None
    )

    # Candidate uses installs/, content uses objects/
    assert "installs" in result_candidate.stdout
    assert "objects" in result_content.stdout


# ---------------------------------------------------------------------------
# Profile persistence and interactions
# ---------------------------------------------------------------------------


def test_profile_survives_reinstall(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Profile entries persist after uninstall + reinstall"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    # Uninstall and reinstall
    ocx.json("uninstall", pkg.short)
    ocx.json("install", pkg.short)

    # Profile entry is still there
    result = ocx.json("shell", "profile", "list")
    assert len(result) == 1
    assert result[0]["status"] == "active"


def test_profile_manifest_format(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Verify profile.json on disk has correct structure"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())

    assert profile["version"] == 1
    assert isinstance(profile["packages"], list)
    assert len(profile["packages"]) == 1
    entry = profile["packages"][0]
    assert "identifier" in entry
    assert "mode" in entry
    assert entry["mode"] in ("current", "candidate", "content")
    # Candidate mode should not have content_digest
    assert "content_digest" not in entry


def test_profile_manifest_content_digest(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Content-mode entries store content_digest in profile.json"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--content", pkg.short)

    profile_path = Path(ocx.env["OCX_HOME"]) / "profile.json"
    profile = json.loads(profile_path.read_text())

    entry = profile["packages"][0]
    assert entry["mode"] == "content"
    assert "content_digest" in entry
    assert entry["content_digest"].startswith("sha256:")


def test_uninstall_preserves_profile_entry(
    ocx: OcxRunner, published_package: PackageInfo
):
    """uninstall doesn't touch profile.json; entry becomes broken"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    ocx.json("uninstall", pkg.short)

    # Profile entry still exists but is broken
    result = ocx.json("shell", "profile", "list")
    assert len(result) == 1
    assert result[0]["status"] == "broken"


# ---------------------------------------------------------------------------
# Uninstall/deselect profile warnings
# ---------------------------------------------------------------------------


def test_uninstall_warns_about_profile(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Uninstalling a candidate-mode profiled package emits actionable warning"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", pkg.short)

    result = ocx.run("uninstall", pkg.short, check=True, log_level="warn")
    stderr = result.stderr.lower()
    assert "shell profile" in stderr
    assert "candidate mode" in stderr
    assert "ocx install" in stderr
    assert "ocx shell profile remove" in stderr


def test_deselect_warns_about_profile(
    ocx: OcxRunner, published_package: PackageInfo
):
    """Deselecting a current-mode profiled package emits actionable warning"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)
    ocx.json("shell", "profile", "add", "--current", pkg.short)

    result = ocx.run("deselect", pkg.short, check=True, log_level="warn")
    stderr = result.stderr.lower()
    assert "shell profile" in stderr
    assert "current mode" in stderr
    assert "ocx select" in stderr
    assert "ocx shell profile add --candidate" in stderr


def test_uninstall_deselect_warns_current_profile(
    ocx: OcxRunner, published_package: PackageInfo
):
    """uninstall -d warns about current-mode profile entries"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)
    ocx.json("shell", "profile", "add", "--current", pkg.short)

    result = ocx.run(
        "uninstall", "-d", pkg.short, check=True, log_level="warn"
    )
    stderr = result.stderr.lower()
    assert "current mode" in stderr
    assert "ocx select" in stderr


def test_uninstall_purge_warns_content_profile(
    ocx: OcxRunner, published_package: PackageInfo
):
    """uninstall --purge warns about content-mode profile entries"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--content", pkg.short)

    result = ocx.run(
        "uninstall", "--purge", pkg.short, check=True, log_level="warn"
    )
    stderr = result.stderr.lower()
    assert "content mode" in stderr
    assert "ocx install" in stderr


def test_clean_protects_profiled_content_objects(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx clean skips objects referenced by content-mode profile entries"""
    pkg = published_package
    ocx.json("install", pkg.short)
    ocx.json("shell", "profile", "add", "--content", pkg.short)

    # Remove the candidate symlink so refs/ becomes empty
    ocx.json("uninstall", pkg.short)

    # Clean should keep the object because profile references it
    ocx.json("clean")

    # Profile entry should still be active (object preserved)
    result = ocx.json("shell", "profile", "list")
    assert len(result) == 1
    assert result[0]["status"] == "active"
