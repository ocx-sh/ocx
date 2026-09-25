# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`rustdoc_diagnostics_aspect`: rustdoc's warnings as a cached file.

`task rust:doc:ratchet` reads `<target>.rustdoc.diagnostics`, one per non-test
Rust crate under `//crates`, through `scripts/lint_ratchet.py --bep`. No
rules_rust setting captures rustdoc diagnostics, and Bazel prints an action's
stderr only when it executes, so a console scrape reads nothing on a cache hit
(plan_bazel_cargo_port.md C-020).

The action is `rust_doc`'s own: `rustdoc_compile_action` is private
(`rust/private` declares no `visibility()`). A rules_rust bump that renames or
re-shapes it fails at analysis. One that changes what it reads off `ctx.attr`
need not: an aspect's `ctx.attr` holds only the aspect's own attributes, so any
rule attribute the function starts reading is silently absent here. Today that
drops `crate_features` (guarded below), `data` and `version`; the crate's
`CARGO_PKG_*` environment still arrives, through `crate_info.rustc_env`.
"""

load("@rules_rust//rust/private:common.bzl", "rust_common")
load("@rules_rust//rust/private:rustc.bzl", "error_format")
load("@rules_rust//rust/private:rustdoc.bzl", "rustdoc_compile_action")
load("@rules_rust//rust/private:utils.bzl", "find_toolchain")

visibility("private")

# The features rustdoc may be run without. `construct_arguments` reads
# `crate_features` off `ctx.attr`, which on an aspect is the aspect's own, so
# no `--cfg feature=` reaches rustdoc. That matches `cargo doc --no-deps`,
# which enables only default features, and no workspace crate has one — these
# two are test-only switches cargo doc never sees either. Any other feature
# would be documented by cargo and not here, so the crate's diagnostics action
# fails instead — an action, not analysis: the aspect sits on every bare
# `build`, and only a build asking for the `rustdoc_diagnostics` group runs it.
_UNDOCUMENTED_FEATURES = ["__testing", "__test_scaffolding"]

# rules_rust's `error_format` setting rule, re-exported for root
# `BUILD.bazel`: buildifier's `bzl-visibility` refuses a BUILD file loading
# `rust/private` directly, and the aspect below needs its own json-valued
# instance of it.
json_error_format = error_format

def _rustdoc_diagnostics_aspect_impl(target, ctx):
    if not target.label.package.startswith("crates/") or rust_common.crate_info not in target:
        return []
    crate_info = target[rust_common.crate_info]
    if crate_info.is_test:
        return []
    diagnostics = ctx.actions.declare_file(ctx.label.name + ".rustdoc.diagnostics")
    extra = [f for f in getattr(ctx.rule.attr, "crate_features", []) if f not in _UNDOCUMENTED_FEATURES]
    if extra:
        message = ("{} enables crate_features {}, which rustdoc_diagnostics_aspect cannot pass to rustdoc " +
                   "(an aspect does not see the rule's crate_features). Pass them explicitly as `--cfg` " +
                   "flags in rustdoc_diagnostics.bzl, or add them to _UNDOCUMENTED_FEATURES if cargo doc " +
                   "does not enable them either.").format(target.label, extra)
        ctx.actions.run_shell(
            mnemonic = "RustdocDiagnostics",
            progress_message = "Rustdoc diagnostics %{label}",
            outputs = [diagnostics],
            command = 'printf "%s\\n" "$1" >&2; exit 1',
            arguments = [message],
        )
        return [OutputGroupInfo(rustdoc_diagnostics = depset([diagnostics]))]

    html = ctx.actions.declare_directory(ctx.label.name + ".rustdoc_html")

    # `cargo doc --no-deps` under the `RUSTDOCFLAGS=--cap-lints warn` of the
    # cargo ratchet run this aspect replaced. The cap keeps every warning a
    # warning: without it a deny-level lint fails the action and the file is
    # never written. A binary crate gets cargo's two extra flags for binaries.
    flags = ctx.actions.args()
    flags.add("--cap-lints=warn")
    if crate_info.type == "bin":
        flags.add("--document-private-items")
        flags.add("-Arustdoc::private-intra-doc-links")

    action = rustdoc_compile_action(
        ctx = ctx,
        toolchain = find_toolchain(ctx),
        crate_info = crate_info,
        output = html,
        rustdoc_flags = flags,
    )

    # The process_wrapper flags are `arguments[0]`; `--stderr-file` there
    # sends rustdoc's stderr (its JSON diagnostics) to the declared file,
    # the mechanism `rust_clippy_action` uses for `.clippy.out`.
    wrapper = ctx.actions.args()
    wrapper.add("--stderr-file", diagnostics)
    ctx.actions.run(
        mnemonic = "RustdocDiagnostics",
        progress_message = "Rustdoc diagnostics %{label}",
        outputs = [html, diagnostics],
        executable = action.executable,
        inputs = action.inputs,
        env = action.env,
        arguments = [wrapper] + action.arguments,
        tools = action.tools,
        toolchain = Label("@rules_rust//rust:toolchain_type"),
    )
    return [OutputGroupInfo(rustdoc_diagnostics = depset([diagnostics]))]

rustdoc_diagnostics_aspect = aspect(
    implementation = _rustdoc_diagnostics_aspect_impl,
    attrs = {
        # `rustdoc_compile_action` hands `ctx.attr` to `construct_arguments`,
        # whose `_get_rustc_env` reads `attr.name` unguarded; without the
        # attribute analysis fails. On a command-line aspect its value is
        # `--aspects_parameters`' default, "" (probed at analysis), and it
        # does not matter: `construct_arguments` then applies
        # `crate_info.rustc_env`, which carries the rule's `CARGO_PKG_NAME`
        # and `CARGO_PKG_VERSION` (aquery: `ocx_util`, `0.6.2`).
        "name": attr.string(),
        # JSON only through this attribute: `construct_arguments` already adds
        # `--error-format` from it, so the flag in `rustdoc_flags` would be a
        # duplicate rustdoc rejects.
        "_error_format": attr.label(default = Label("//:rustdoc_error_format")),
        "_process_wrapper": attr.label(
            default = Label("@rules_rust//util/process_wrapper"),
            executable = True,
            cfg = "exec",
        ),
    },
    fragments = ["cpp"],
    required_providers = [rust_common.crate_info],
    toolchains = [
        str(Label("@rules_rust//rust:toolchain_type")),
        config_common.toolchain_type("@bazel_tools//tools/cpp:toolchain_type", mandatory = False),
    ],
)
