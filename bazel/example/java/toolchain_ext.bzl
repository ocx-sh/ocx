"""
Provides the module extension for loading OCX packages as Bazel repositories.
"""

load("@rules_ocx//ocx:extension_builder.bzl", "build_extension")

def _ocx_package_images_repo_impl(repository_ctx, layer_path, _config_path):
    repository_ctx.file(
        "BUILD.bazel",
        """
load("@rules_java//java/toolchains:java_runtime.bzl", "java_runtime")

java_runtime(
    name = "config",
    java = glob(["{layer_path}/bin/java", "{layer_path}/bin/java.exe"], allow_empty = True)[0],
    srcs = glob(["{layer_path}/bin/**", "{layer_path}/conf/**", "{layer_path}/include/**", "{layer_path}/lib/**"], allow_empty = True),
    version = {java_version},
    visibility = ["//visibility:public"]
)

""".format(
            layer_path = layer_path,
            java_version = repository_ctx.attr.java_version,
        ),
    )

def _ocx_package_repo_impl(repository_ctx, images_by_platform):
    images_select_inner = {}
    build_file_content = ""
    for platform, image_repo_name in images_by_platform.items():
        platform_config_setting_target = "@rules_ocx//ocx/private:{}".format(platform)
        images_select_inner[platform_config_setting_target] = "@" + image_repo_name + "//:config"
        build_file_content += """
toolchain(
    name = "{name}_runtime",
    toolchain = "{config_target}",
    toolchain_type = "@bazel_tools//tools/jdk:runtime_toolchain_type",
    exec_compatible_with = ["{platform}"],
    visibility = ["//visibility:public"]
)

toolchain(
    name = "{name}_bootstrap_runtime",
    toolchain = "{config_target}",
    toolchain_type = "@bazel_tools//tools/jdk:bootstrap_runtime_toolchain_type",
    exec_compatible_with = ["{platform}"],
    visibility = ["//visibility:public"]
)
""".format(
            name = repository_ctx.original_name + "_" + platform,
            config_target = "@" + image_repo_name + "//:config",
            platform = platform_config_setting_target,
        )

    build_file_content += """
"""

    repository_ctx.file("BUILD.bazel", build_file_content)

ocx_java, ocx_java_package_repo, ocx_java_package_images_repo = build_extension(
    _ocx_package_repo_impl,
    _ocx_package_images_repo_impl,
    additional_attrs = {"java_version": attr.int(mandatory = True)},
)
