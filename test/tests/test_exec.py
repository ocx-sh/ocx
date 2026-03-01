from src import OcxRunner, PackageInfo


def test_exec_runs_correct_binary(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx exec <pkg> -- hello"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    result = ocx.plain("exec", pkg.short, "--", "hello")
    assert result.stdout.strip() == pkg.marker


def test_exec_runs_correct_version(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """ocx install <v1>; ocx install <v2>; ocx exec <v1> -- hello; ocx exec <v2> -- hello"""
    v1, v2 = published_two_versions
    ocx.plain("install", v1.short)
    ocx.plain("install", v2.short)

    result_v1 = ocx.plain("exec", v1.short, "--", "hello")
    assert result_v1.stdout.strip() == v1.marker

    result_v2 = ocx.plain("exec", v2.short, "--", "hello")
    assert result_v2.stdout.strip() == v2.marker
