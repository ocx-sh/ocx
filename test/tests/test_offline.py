from src import OcxRunner, PackageInfo


def test_find_fails_offline_when_not_installed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx --offline find <pkg>  (not installed, expects failure)"""
    pkg = published_package
    result = ocx.plain("--offline", "find", pkg.short, check=False)
    assert result.returncode != 0


def test_find_succeeds_offline_when_installed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx --offline find <pkg>"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    result = ocx.plain("--offline", "find", pkg.short)
    assert result.returncode == 0


def test_exec_works_offline_when_installed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx --offline exec <pkg> -- hello"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    result = ocx.plain("--offline", "exec", pkg.short, "--", "hello")
    assert result.stdout.strip() == pkg.marker
