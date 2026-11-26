# Research: Bazel module-extension facts, the lockfile and update targets

Date: 2026-11-26 · Expires: 2027-02-26 · Triggered by: `/hex-architect` high, "Package locking and update targets in rules_ocx", goal `.agents/goals/bazel-rbe-rules-ocx.md` · Domain: bazel

Axis: what `reproducible`, facts and `--lockfile_mode` guarantee in Bazel 9 (8.x where it differs), and what a `bazel run` target may do to a lockfile.
Tags: `observed` (run on this host, Linux x86_64, Bazel 9.2.0, `--ignore_all_rc_files`, scratch output root), `documented` (URL), `read in source` (URL; Bazel sources read at master, not at the 9.2.0 tag), `not established`.
Consumer: rules_ocx ADR `.agents/adr/adr_0003_package_locking.md` in the rules_ocx worktree.

## 1. Facts

- `module_ctx.facts` is the dict an earlier run returned, `{}` otherwise; it allows key lookup and `in` only, no enumeration (`documented`, https://bazel.build/rules/lib/builtins/module_ctx).
- `extension_metadata(facts = …)` is "always persisted in the lockfile" and survives a change of the extension's code (`documented`, https://bazel.build/external/extension). Bazel may shallow-merge two facts dicts on a lockfile merge conflict.
- Allowed value types: string, None, bool, number, list, dict with string keys; depth at most 7; no size limit found (`read in source`, `Facts.java`).
- The returned dict replaces the stored one. A fact the run does not return is dropped (`read in source`, `SingleExtensionEvalFunction.java:303-350`; `observed`: a removed declaration lost its fact).
- `facts_version` on `module_extension` discards every stored fact when it changes. It is the only documented reset (`documented`, https://bazel.build/external/extension). Too coarse for one package.
- Facts are said to need Bazel 8.5 or later (`documented` by a third party, https://aspect.build/blog/bazel-facts-api.md).
- Known users cache checksums keyed by a stable version: rules_go, rules_js, rules_swc, rules_nodejs (same source). No OCI ruleset found that uses facts.

## 2. `--lockfile_mode`

| Mode | Facts read | Facts written | A changed or new fact | Source |
|---|---|---|---|---|
| `update` (default) | yes | yes | written | `read in source`, `observed` |
| `refresh` | yes | yes | written; `refresh` refreshes registry data, not facts | `documented` https://bazel.build/external/lockfile, `observed` |
| `error` | yes (8.6.0 and later: workspace lockfile only) | no | fails: "MODULE.bazel.lock is no longer up-to-date because the extension '…' has changed its facts: … Please run `bazel mod deps --lockfile_mode=update`" | `observed`; 8.6.0 change from the release branch log |
| `off` | none | no | the extension resolves everything again | `read in source` `BazelLockFileFunction.java:83-84`, `observed` |

- Under `error`, a fact miss still runs the extension (and its requests) before failing (`observed`).
- No flag and no `bazel mod` subcommand re-resolves a fact. A value read with `module_ctx.getenv` is a recorded input: a changed `--repo_env` re-runs the extension, and the old facts are still passed in, so the extension's own code decides to ignore one (`read in source`, `StarlarkBaseExternalContext.getEnvironmentValue`; `observed`).
- `--repo_env` itself is not written to the lockfile; only the variables read are recorded inputs (`documented` https://bazel.build/external/lockfile).

## 3. `reproducible = True`

- The extension is left out of `MODULE.bazel.lock`; its result is cached in the hidden lockfile in the output base and reused when the `.bzl` digest, the tags, the recorded inputs and (for reproducible extensions) the facts are unchanged (`documented` https://bazel.build/external/extension; `read in source` `SingleExtensionEvalFunction.java:372-447`).
- So it runs once per changed input or new output base, not once per command (`read in source`). `observed`: editing the extension re-ran it; every fact hit made no request.
- rules_oci instead marks the extension non-reproducible when a pull has no digest, and stores the whole result in `MODULE.bazel.lock`; it writes no facts (`read in source`, https://raw.githubusercontent.com/bazel-contrib/rules_oci/main/oci/extensions.bzl lines 42-46, 83-87). A tag pull warns "Since tags are mutable, this is not reproducible." (`documented`, https://github.com/bazel-contrib/rules_oci/blob/main/docs/pull.md).

## 4. `bazel run` and a nested `bazel`

- The client releases the output-base lock before it starts the binary, except under `--batch` (`read in source`, `blaze.cc`, `RunCommand.java`). An old 2018 thread says the opposite (https://groups.google.com/g/bazel-discuss/c/VQYCr2D08l0/m/tQyy22_NFgAJ); the current source and the observation below contradict it.
- `observed`: a `bazel run` target that runs `bazel mod deps --lockfile_mode=update --repo_env=OCX_RULES_UPDATE=a` in `BUILD_WORKSPACE_DIRECTORY`, with the same output root, did not deadlock; only `a` was resolved; the lockfile diff was the fact of `a`; a `print()` of the extension reached stderr as a `DEBUG:` line.
- The binary gets `BUILD_WORKSPACE_DIRECTORY`, `BUILD_WORKING_DIRECTORY`, `BUILD_EXECROOT`, `BUILD_ID` and the client's environment; nothing names the bazel binary or the startup options (`documented`, https://bazel.build/docs/user-manual).
- Prior art: aspect `write_source_files` writes source files from a `bazel run` target without calling bazel; rules_jvm_external (`REPIN=1 bazel run @maven//:pin`) and rules_rust (`CARGO_BAZEL_REPIN`, allow-list `CARGO_BAZEL_REPIN_ONLY`) switch the resolve with an environment variable (`documented` by search results only). No ruleset found whose run target edits `MODULE.bazel.lock` itself.

## 5. Windows runfiles manifest

- `--enable_runfiles` defaults to off on Windows; only the manifest is written (`documented`, https://bazel.googlesource.com/bazel/+/3e50e3565b1d39a8ab82d817bca9bc2eb3ca8fc1). Not re-checked for 9.2.0.
- The manifest beside a binary is `BIN.runfiles_manifest`; inside a tree it is `BIN.runfiles/MANIFEST` (`not established` on 9.2.0; inferred from a runfiles test).
- Line format since 8.0: `RLOCATION TARGET`. A source path with a space, newline or backslash starts the line with one space and escapes those as `\s`, `\n`, `\b`; the target is not escaped (`documented`, Bazel commit 7407cef3837b, https://bazel.googlesource.com/bazel/+/refs/heads/release-8.0.0rc1%5E). Every Windows claim here is unproven on this host.

## Not established

- Bazel sources at the 9.2.0 tag (read at master); the official 8.5 note for facts.
- A nested `bazel` with startup options other than the outer call's; a Windows `bazel run` update target.
- rules_python, rules_img and rules_distroless lock handling.
