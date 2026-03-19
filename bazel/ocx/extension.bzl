"""
Provides the module extension for loading OCX packages as Bazel repositories.
"""

load("//ocx:extension_builder.bzl", "build_extension")

def _ocx_package_images_repo_impl(repository_ctx, layer_path, config_path):
    config = json.decode(repository_ctx.read(config_path))
    env_config = config.get("env", [])
    repository_ctx.file(
        "BUILD.bazel",
        """
load("@rules_ocx//ocx/private:external_image.bzl", "external_ocx_image")

external_ocx_image(
    name = "image",
    layer_contents = glob(["{layer_path}/**"]),
    env = {env},
    visibility = ["//visibility:public"]
)
""".format(
            layer_path = layer_path,
            env = json.encode(json.encode(env_config)),
        ),
    )

def _ocx_package_repo_impl(repository_ctx, images_by_platform):
    images_select_inner = {}
    for platform, image_repo_name in images_by_platform.items():
        platform_config_setting_target = "@rules_ocx//ocx/private:{}".format(platform)
        images_select_inner[platform_config_setting_target] = "@" + image_repo_name + "//:image"

    repository_ctx.file("BUILD.bazel", """
alias(
    name="{name}",
    actual = select({images}),
    visibility = ["//visibility:public"]
)""".format(
        name = repository_ctx.original_name,
        images = json.encode(images_select_inner),
    ))

ocx, ocx_package_repo, ocx_package_images_repo = build_extension(_ocx_package_repo_impl, _ocx_package_images_repo_impl)
