load("@rules_cc//cc:defs.bzl", "CcInfo", "cc_binary")
load("@rules_cc//cc/common:cc_common.bzl", "cc_common")
load("//ocx/private:providers.bzl", "OcxImageInfo")

def _ocx_launcher_defines_impl(ctx):
    bin_path_in_layer = ctx.attr.bin
    bin_path_in_layer_windows = bin_path_in_layer + ".exe"

    # resolve the actual bin file in the image layer
    bin_path = None
    for runfile in ctx.files.package:
        runfile_in_layer = runfile.path.partition("/layer/")[2]
        if runfile_in_layer == bin_path_in_layer or runfile_in_layer == bin_path_in_layer_windows:
            bin_path = runfile
            break
    if not bin_path:
        fail("bin '{}' not found in image".format(bin_path_in_layer))

    # transform the bin path in order to make it resolvable via the runfiles mapping
    bin_path_short = bin_path.short_path
    if bin_path_short.startswith("../"):
        bin_path_short = bin_path_short[3:]
    else:
        bin_path_short = "_main/" + bin_path_short

    # encode the environment variables as a C array of "key=value" strings
    env_array_items = []
    for env_var in ctx.attr.package[OcxImageInfo].env:
        env_array_items.append(json.encode(env_var["key"] + "=" + env_var["value"]))
    env_array = "{" + ", ".join(env_array_items) + "}"


    defines = [
        "BIN_PATH=" + json.encode(bin_path_short),
        "ENV=" + env_array,
    ]
    return [
        CcInfo(
            compilation_context = cc_common.create_compilation_context(
                defines = depset(defines),
            ),
        ),
    ]

_ocx_launcher_defines_lib = rule(
    implementation = _ocx_launcher_defines_impl,
    attrs = {
        "bin": attr.string(mandatory = True),
        "package": attr.label(mandatory = True, allow_files = True, cfg = "exec", providers = [DefaultInfo, OcxImageInfo]),
    },
    doc = """Generates the defines required for the launcher binary to locate the executable in the image layer and to set up the environment variables.
This rule is not meant to be used directly, but rather via the ocx_launcher macro.""",
)

def _ocx_launcher_impl(name, bin, package, **kwargs):
    defines_lib_target = name + "_defines"
    _ocx_launcher_defines_lib(
        name = defines_lib_target,
        bin = bin,
        package = package,
    )
    cc_binary(
        name = name,
        srcs = [Label(":main.cc")],
        data = [package],
        deps = [Label("@rules_cc//cc/runfiles:runfiles"), ":" + defines_lib_target],
        **kwargs
    )

ocx_launcher = macro(
    implementation = _ocx_launcher_impl,
    attrs = {
        "bin": attr.string(mandatory = True, doc = "The path to the executable in the image."),
        "package": attr.label(mandatory = True, allow_files = True, cfg = "exec", configurable = False, providers = [DefaultInfo, OcxImageInfo], doc = "The image / package containing the executable."),
    },
    doc = "Defines a launcher binary that launches the executable specified by 'bin' from the image of 'package'",
)
