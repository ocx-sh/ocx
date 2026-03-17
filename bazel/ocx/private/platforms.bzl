"""
Defines platform-specific handling for OCX.
"""

load("@bazel_skylib//lib:selects.bzl", "selects")

ALL_ARCH = ["aarch64", "x86_64"]
ALL_OS = ["linux", "windows", "macos"]

def generate_platforms():
    """Generates config setting targets for all supported platform combinations."""
    for os in ALL_OS:
        for arch in ALL_ARCH:
            selects.config_setting_group(
                name = build_platform_name(arch, os),
                match_all = [
                    "@platforms//cpu:{}".format(arch),
                    "@platforms//os:{}".format(os),
                ],
            )

_OCI_ARCH_MAPPING = {
    "amd64": "x86_64",
    "arm64": "aarch64",
}

_OCI_OS_MAPPING = {
    "linux": "linux",
    "windows": "windows",
    "darwin": "macos",
}

def platform_from_oci(oci_arch, oci_os):
    """Maps OCI architecture and OS to Bazel platform constraint values.

    Args:
        oci_arch: The OCI architecture string, e.g. "amd64" or "arm64".
        oci_os: The OCI OS string, e.g. "linux", "windows", or "darwin".
    Returns:
        A tuple of (arch, os) of Bazel platform constraint values if the mapping is successful, or None if the
    """
    arch = _OCI_ARCH_MAPPING.get(oci_arch)
    os = _OCI_OS_MAPPING.get(oci_os)
    if not arch or not os:
        return None
    return (arch, os)

def build_platform_name(arch, os):
    """Builds an internal platform name based on the architecture and OS."""
    return "platform_{}_{}".format(arch, os)
