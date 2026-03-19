"""Run a java binary with jdk21 and jdk25"""

multijdk_transition = transition(
    implementation = lambda settings, attrs: {
        "21": {"//command_line_option:extra_toolchains": "@jdk_corretto21//..."},
        "25": {"//command_line_option:extra_toolchains": "@jdk_corretto25//..."},
    },
    inputs = [],
    outputs = ["//command_line_option:extra_toolchains"],
)

def _run_multijdk_impl(ctx):
    bin_21 = ctx.split_attr.bin["21"][DefaultInfo].files_to_run
    bin_25 = ctx.split_attr.bin["25"][DefaultInfo].files_to_run

    ctx.actions.run_shell(
        command = "$1 > $3 && $2 >> $3",
        outputs = [ctx.outputs.out],
        inputs = [],
        tools = [bin_21, bin_25],
        arguments = [ctx.actions.args().add_all([bin_21.executable, bin_25.executable, ctx.outputs.out])],
    )

run_multijdk = rule(
    implementation = _run_multijdk_impl,
    attrs = {
        "bin": attr.label(mandatory = True, allow_files = True, cfg = multijdk_transition),
        "out": attr.output(mandatory = True),
    },
)
