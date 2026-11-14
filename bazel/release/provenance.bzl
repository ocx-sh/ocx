"""`release_provenance`: the release build's stand-in for `build.rs`'s real provenance.

One rustc env file (`KEY=VALUE` lines) for `//crates/ocx_cli:ocx_cli`'s
`rustc_env_files` under `--define=ocx_provenance=release`:

* The workspace status (`ctx.info_file`, `bazel-out/stable-status.txt`):
  `bazel/release/workspace_status.py` prints every git, CI and build-channel
  value as `STABLE_OCX_<VAR> <value>`; the prefix is stripped here. A value the
  script did not print is absent, so `option_env!` sees None and the report
  omits the field, as `build.rs`'s pass-through does.
* The rust toolchain of the target configuration: the exact triple (the
  platform selects in `BUILD.bazel` cannot tell musl from gnu), the rustc
  version and the debug flag from the compilation mode. rules_rust applies an
  env file after `rustc_env`, so these win over the dict.

Stable keys only: Bazel keys an action on `stable-status.txt` and treats
`volatile-status.txt` as never changing, so a value read from the volatile half
would go stale in the local action cache. The action is `no-remote-cache`:
nothing that read the workspace status reaches the shared cache.
"""

visibility("private")

def _release_provenance_impl(ctx):
    toolchain = ctx.toolchains["@rules_rust//rust:toolchain_type"]
    out = ctx.actions.declare_file(ctx.label.name + ".env")
    ctx.actions.run_shell(
        inputs = [ctx.info_file],
        outputs = [out],
        # POSIX sed + printf only. The status is `KEY value` per line; a value
        # holds no newline (workspace_status.py refuses it).
        command = "{ sed -n 's/^STABLE_OCX_\\([A-Za-z0-9_]*\\) \\(.*\\)$/\\1=\\2/p' '%s'; printf 'VERGEN_CARGO_TARGET_TRIPLE=%%s\\nVERGEN_RUSTC_SEMVER=%%s\\nVERGEN_CARGO_DEBUG=%%s\\n' '%s' '%s' '%s'; } > '%s'" % (
            ctx.info_file.path,
            toolchain.target_triple.str,
            toolchain.version,
            "true" if ctx.var["COMPILATION_MODE"] == "dbg" else "false",
            out.path,
        ),
        execution_requirements = {"no-remote-cache": "1"},
        mnemonic = "OcxProvenance",
        progress_message = "Writing release provenance for %{label}",
    )
    return [DefaultInfo(files = depset([out]))]

release_provenance = rule(
    implementation = _release_provenance_impl,
    doc = "The release build's rustc env file: workspace-status provenance plus the target toolchain's triple and rustc.",
    toolchains = ["@rules_rust//rust:toolchain_type"],
)
