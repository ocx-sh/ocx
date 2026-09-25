# Research: rules_rust 0.74 clippy aspect, rustdoc and doctests

For [`plan_bazel_cargo_port.md`](./plan_bazel_cargo_port.md) (PR ocx-sh/ocx#527, issue ocx-sh/ocx#531).
Pinned: `rules_rust 0.74.0` (`MODULE.bazel`), Bazel 9.2.0. Written 2026-09-25.

Verified against the fetched source in the local output base
(`external/rules_rust+/rust/private/{clippy,rustdoc,rustdoc_test,rustc}.bzl`), not only upstream docs.

## A. Clippy aspect

- Aspect: `@rules_rust//rust:defs.bzl%rust_clippy_aspect`. Runs on anything carrying `CrateInfo` or
  `TestCrateInfo` — `rust_library`, `rust_binary`, `rust_test` (incl. `crate =` unit tests). Skips
  external repositories. Opt-out tags: `no_clippy`, `no_lint`, `nolint`, `noclippy`.
- Output groups: `clippy_checks` (`.clippy.ok` marker / `.clippy.out`), and `clippy_output`, which is
  `<name>.clippy.diagnostics` (sibling of the crate output) when
  `--@rules_rust//rust/settings:clippy_output_diagnostics=true`.
- `clippy.bzl:_clippy_aspect_impl`: `cap_at_warnings = clippy_out != None or clippy_diagnostics != None`
  → `--cap-lints=warn`, so a capturing run never fails on a lint; the action (mnemonic `Clippy`)
  succeeds and is an ordinary cacheable action. A real compile error still fails it.
- The diagnostics file is written through the process_wrapper's `--output-file` (the crate_info's
  `rustc_output` is swapped for it) with `use_json_output = True`; with
  `--@rules_rust//rust/settings:clippy_error_format=json` the process_wrapper passes rustc's JSON
  through (`--rustc-output-format json`). Content: rustc-native JSON diagnostics, one per line
  (`$message_type: "diagnostic"`), not cargo's `compiler-message` wrapper; artifact notifications are
  filtered by the wrapper.
- Flags: `--@rules_rust//rust/settings:clippy_flag=<flag>` (repeatable) / `clippy_flags`. If any
  clippy flag (or lint file) is set, the default `-Dwarnings` is **not** added; otherwise it is.
  `clippy.toml` setting must name a file called `clippy.toml`/`.clippy.toml` (repo has none).
- `incompatible_change_clippy_error_format` defaults true → the aspect honours `clippy_error_format`.
- Pitfall: rules_rust#2510 — diagnostics file collision when unsandboxed; this repo builds sandboxed.

## B. Workspace lints

rules_rust does not read Cargo `[lints]` / `[workspace.lints]`. Available: `rust_lint_config`
(`rust/private/lints.bzl`, `LintsInfo`, set per target via `lint_config`) and `extract_cargo_lints`
(`cargo/private/cargo_lints.bzl`, derives `LintsInfo` from `Cargo.toml`). Both are per-target opt-in;
no global default. This repo's `[workspace.lints]` is `rust.warnings = "deny"` plus an empty clippy
table, and the ratchet's `-W unreachable_pub` is the only extra level — a `clippy_flag` covers it.

## C. rustdoc warnings

- `rust_doc` (`rustdoc.bzl`) runs a plain `ctx.actions.run`; no diagnostics-capture setting exists.
  Warnings are printed by Bazel only when the action executes — a cache hit prints nothing, so console
  scraping cannot feed a ratchet.
- `rustdoc_compile_action(ctx, toolchain, crate_info, lints_info, output, rustdoc_flags, is_test, …)`
  returns `struct(executable, inputs, env, arguments, tools, …)`; `arguments = args.all`, whose first
  element is the process_wrapper flags `Args`. It calls `construct_arguments` without
  `use_json_output`, which then adds `--error-format=<get_error_format(attr, "_error_format")>` —
  so a caller-supplied `--error-format` would be a duplicate; the error format must come through the
  calling rule/aspect's own `_error_format` attribute.
- A capture wrapper is therefore a small custom aspect: load the private `rustdoc_compile_action`,
  point its `_error_format` at a json-valued setting, add `--cap-lints warn`, route stderr to a
  declared `<name>.rustdoc.diagnostics` via process_wrapper `--stderr-file` (the exact mechanism
  `rust_clippy_action` uses for `.clippy.out`), declare the HTML output as a tree artifact.
  Cost: private-API coupling — a rules_rust bump that changes the signature fails loudly at analysis.

## D. rust_doc_test

- Stable toolchain → `_legacy_rust_doc_test_impl`: writes a runner script that invokes
  `rustdoc --test` at test time from runfiles. Test results cache like any test.
- Attrs: `crate` (mandatory), `deps`, `proc_macro_deps` (doctest-only extras; there is no dev-deps
  notion). Sets no `CARGO_*` env of its own beyond what the crate's `rustc_env` carries.
- Windows was historically broken (rules_rust#887); this repo's Bazel unit gate is Linux-only.

## E. Local facts that shape the plan

- Doctests: 24 runnable + 5 `compile_fail` (all `crates/ocx_oci/src/lib.rs`), 0 `no_run`, across 8
  crates; none reads `env!("CARGO_*")`, `include_str!` or a dev-dependency. 20 crates have `src/lib.rs`
  (`ocx_shim` is bin-only).
- Baselines: `clippy-warn-baseline.json` 44 keys, `rustdoc-warn-baseline.json` 148 keys, both
  `<file>::<code>`.

## Sources

- rules_rust 0.74.0: [clippy.bzl](https://github.com/bazelbuild/rules_rust/blob/0.74.0/rust/private/clippy.bzl) ·
  [rustdoc.bzl](https://github.com/bazelbuild/rules_rust/blob/0.74.0/rust/private/rustdoc.bzl) ·
  [rustdoc_test.bzl](https://github.com/bazelbuild/rules_rust/blob/0.74.0/rust/private/rustdoc_test.bzl) ·
  [rustc.bzl](https://github.com/bazelbuild/rules_rust/blob/0.74.0/rust/private/rustc.bzl) ·
  [settings/BUILD.bazel](https://github.com/bazelbuild/rules_rust/blob/0.74.0/rust/settings/BUILD.bazel) ·
  [lints.bzl](https://github.com/bazelbuild/rules_rust/blob/0.74.0/rust/private/lints.bzl) ·
  [cargo_lints.bzl](https://github.com/bazelbuild/rules_rust/blob/0.74.0/cargo/private/cargo_lints.bzl) ·
  [process_wrapper options.rs](https://github.com/bazelbuild/rules_rust/blob/0.74.0/util/process_wrapper/options.rs)
- Issues: [rules_rust#2510](https://github.com/bazelbuild/rules_rust/issues/2510) ·
  [rules_rust#887](https://github.com/bazelbuild/rules_rust/issues/887)
- [Bazel prints action diagnostics only when the action runs](https://fzakaria.com/2025/06/10/bazel-knowledge-diagnostic-messages-only-on-failure)
