"""
Defines the external_ocx_image rule for representing an OCI image layer in Bazel.
Targets of this rule are instantiated via the ocx_package_images_repo repository_rule
"""
load("//ocx/private:providers.bzl", "OcxImageInfo")

def _external_ocx_image_impl(ctx):
    env = json.decode(ctx.attr.env)
    return [
        OcxImageInfo(env = env),
        DefaultInfo(files = depset(ctx.files.layer_contents)),
    ]

external_ocx_image = rule(
    implementation = _external_ocx_image_impl,
    attrs = {
        "layer_contents": attr.label_list(allow_files = True),
        "env": attr.string(),
    },
    provides = [OcxImageInfo, DefaultInfo],
)
