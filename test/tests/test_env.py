from src import OcxRunner, PackageInfo, registry_dir


def test_env_path_contains_bin(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx env <pkg>"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    env_result = ocx.json("env", pkg.short)
    path_entry = next(e for e in env_result["entries"] if e["key"] == "PATH")
    assert "/bin" in path_entry["value"] or "\\bin" in path_entry["value"]


def test_env_constant_contains_content_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx env <pkg> — constant var points to content dir"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("env", pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert registry_dir(ocx.registry) in home_entry["value"]
    # CAS layout: packages/{registry}/sha256/{prefix}/{suffix}/content
    assert "packages" in home_entry["value"]


def test_env_candidate_uses_symlink_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx env --candidate <pkg>"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("env", "--candidate", pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert f"candidates/{pkg.tag}" in home_entry["value"] or f"candidates\\{pkg.tag}" in home_entry["value"]


def test_shell_env_outputs_export_statements(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx shell env <pkg>"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    result = ocx.plain("shell", "env", pkg.short)
    assert (
        "export PATH=" in result.stdout
        or "set PATH=" in result.stdout
        or "$env:PATH" in result.stdout
    )
