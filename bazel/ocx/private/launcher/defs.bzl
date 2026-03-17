"""Provides macro for creating launcher for binaries inside of an package image"""

load("@rules_cc//cc:defs.bzl", "CcInfo", "cc_binary")
load("@rules_cc//cc/common:cc_common.bzl", "cc_common")
load("//ocx/private:providers.bzl", "OcxImageInfo")

def _get_bin_path_in_package(bin_path_in_layer, package_files):
    bin_path_in_layer_windows = bin_path_in_layer + ".exe"

    # resolve the actual bin file in the image layer
    bin_path = None
    for runfile in package_files:
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
    return bin_path_short

def _ocx_launcher_defines_impl(ctx):
    bin_path_short = _get_bin_path_in_package(ctx.attr.bin, ctx.files.package)

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
        "package": attr.label(mandatory = True, allow_files = True, providers = [DefaultInfo, OcxImageInfo]),
    },
    doc = """Generates the defines required for the launcher binary to locate the executable in the image layer and to set up the environment variables.
This rule is not meant to be used directly, but rather via the ocx_launcher macro.""",
)

_SH_TOOLCHAIN_TYPE = "@rules_shell//shell:toolchain_type"

def _ocx_launcher_script_impl(ctx):
    shell = ctx.toolchains[_SH_TOOLCHAIN_TYPE].path
    shebang = "#!{}".format(shell)
    bin_path_short = _get_bin_path_in_package(ctx.attr.bin, ctx.files.package)
    script_file = ctx.actions.declare_file(ctx.label.name + ".sh")

    env_array_items = []
    for env_var in ctx.attr.package[OcxImageInfo].env:
        env_array_items.append("export " + json.encode(env_var["key"] + "=" + env_var["value"]))
    env_array = "\n".join(env_array_items)

    ctx.actions.expand_template(
        output = script_file,
        template = ctx.file._template,
        is_executable = True,
        substitutions = {
            "${shebang}": shebang,
            "${bin_path_short}": json.encode(bin_path_short),
            "${env}": env_array,
        },
    )
    return [DefaultInfo(executable = script_file, runfiles = ctx.runfiles(files = ctx.files.package))]

_ocx_launcher_script = rule(
    implementation = _ocx_launcher_script_impl,
    executable = True,
    attrs = {
        "bin": attr.string(mandatory = True),
        "package": attr.label(mandatory = True, allow_files = True, providers = [DefaultInfo, OcxImageInfo]),
        "_template": attr.label(allow_single_file = [".sh"], default = Label(":launcher.sh")),
        "_runfiles_lib": attr.label(allow_files = True, default = Label("@rules_shell//shell/runfiles:runfiles")),
    },
    toolchains = [_SH_TOOLCHAIN_TYPE],
)

def _ocx_launcher_impl(name, bin, package, **kwargs):
    defines_lib_target = name + "_defines"
    _ocx_launcher_defines_lib(
        name = defines_lib_target,
        bin = bin,
        package = package,
    )
    binary_launcher_target = name + "_bin"
    cc_binary(
        name = binary_launcher_target,
        srcs = [Label(":launcher.cc")],
        data = [package],
        deps = [Label("@rules_cc//cc/runfiles:runfiles"), ":" + defines_lib_target],
        **kwargs
    )

    script_launcher_target = name + "_script"
    _ocx_launcher_script(
        name = script_launcher_target,
        bin = bin,
        package = package,
    )

    native.alias(
        name = name,
        actual = select({
            "@platforms//os:windows": ":" + binary_launcher_target,
            "//conditions:default": ":" + script_launcher_target,
        }),
        **kwargs
    )

ocx_launcher = macro(
    implementation = _ocx_launcher_impl,
    attrs = {
        "bin": attr.string(mandatory = True, doc = "The path to the executable in the image."),
        "package": attr.label(mandatory = True, allow_files = True, configurable = False, providers = [DefaultInfo, OcxImageInfo], doc = "The image / package containing the executable."),
    },
    doc = "Defines a launcher binary that launches the executable specified by 'bin' from the image of 'package'",
)
