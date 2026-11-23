# Research: Bazel 9 mechanics fact-check

## Metadata

**Date:** 2026-11-18
**Lane:** disputed-fact check (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** bazel.build docs and Bazel source on master as cited; read through a summarising fetch, not run.
**Expires:** 2027-05-18
**Caveat:** findings only, no recommendation; items marked UNVERIFIED had no primary source read.

### Findings

**Q1a. Facts exist.**
- `module_ctx.facts` is read-only. It returns the JSON-like dict passed as `facts` to `extension_metadata` in a previous run, or `{}`. [module_ctx 9.2](https://bazel.build/versions/9.2.0/rules/lib/builtins/module_ctx.md)
- `extension_metadata(root_module_direct_deps, root_module_direct_dev_deps, reproducible=False, facts={})`. The `facts` parameter is documented as a dict of strings; the doc body says JSON-like. [module_ctx](https://bazel.build/rules/lib/builtins/module_ctx)
- Purpose: "universally true facts such as the hashes of artifacts in an immutable repository". Use it for key-value storage only, with the key uniquely determining the value. Bazel may shallow-merge facts dicts on lockfile merge conflicts, as with `dict.update()`. A single fixed top-level key opts out of merging. The doc warns the value may come from a different extension version with a different schema.
- Persistence is in `MODULE.bazel.lock`. [extension docs](https://bazel.build/external/extension) say facts are not invalidated when extension code changes. Raising `facts_version` makes Bazel discard facts recorded under a different version.
- Extensions that depend on unchecksummed network data cannot be marked reproducible unless they use facts. [extension docs](https://bazel.build/external/extension)
- Introduced in Bazel 8.5, per an Aspect post that says it behaves the same across 8.x and 9.x. [aspect.build](https://aspect.build/blog/bazel-facts-api.md) This is a secondary source; the official `module_ctx` page has no "since" note. UNVERIFIED against release notes.
- Aspect post limits: key lookup and `in` only, no iteration (`keys()`, `items()`, `len()`). It only helps if the lockfile is committed.
- The 9.2 `module_ctx` doc does not mention `facts_version`. The [extension docs](https://bazel.build/external/extension) do. The two official pages are inconsistent on this.
- No size limit is documented.

**Q1b. Lockfile contents and re-execution.**
- Each lockfile entry holds `bzlTransitiveDigest`, `usagesDigest`, recorded inputs, `generatedRepoSpecs`, and optional `moduleExtensionMetadata`. [lockfile docs](https://bazel.build/external/lockfile)
- With `reproducible=True` the extension gets no `MODULE.bazel.lock` entry. [module_ctx](https://bazel.build/rules/lib/builtins/module_ctx)
- Source: reproducible results go to a hidden lockfile in the output base. The workspace lockfile gets non-reproducible results, registry hashes, yanked versions and facts. [BazelLockFileModule.java](https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelLockFileModule.java)
- The [extension docs](https://bazel.build/external/extension) say the hidden cache persists across server restarts.
- The implementation function is skipped only when the lockfile mode is not OFF, a locked entry exists, and all of these match: bzl digest, usages digest, recorded inputs. A reproducible extension with non-empty facts also needs facts equal to the hidden lockfile's facts. The skip then reuses the locked `generatedRepoSpecs`. [SingleExtensionEvalFunction.java](https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/SingleExtensionEvalFunction.java)
- Consequence for a fresh machine with a committed, current lockfile:
  - A non-reproducible extension is not re-run.
  - A reproducible extension has no workspace-lockfile entry and an empty hidden lockfile, so it is re-evaluated (the code returns null and runs it).
  - In `--lockfile_mode=error`, a missing or stale non-reproducible entry fails with `BAD_LOCKFILE`.
- That consequence is inferred from the source summary, not observed running. UNVERIFIED empirically.

**Q2. Download inside an action.**
- `ctx.actions` has these methods: `args, declare_directory, declare_file, declare_symlink, do_nothing, expand_template, map_directory, run, run_shell, symlink, template_dict, write`. There is no download action. [actions](https://bazel.build/rules/lib/builtins/actions)
- rules_img README (`github.com/bazel-contrib/rules_img`):
  - Lazy push streams missing blobs from Bazel's remote cache.
  - Build actions fetch base layers from registries, optionally through an OCI distribution gateway.
  - Only the manifest and config are fetched at analysis time.
  - The README never names the executable that downloads, or how actions obtain it. It mentions an `img` CLI and an `img_tool` directory. UNVERIFIED which binary and how it is obtained.
- rules_oci: not read. UNVERIFIED.
- `run(executable=...)` accepts a File, a string, or a FilesToRunProvider. The docs do not define string resolution. [actions](https://bazel.build/versions/9.2.0/rules/lib/builtins/actions.md)
  - Source: the string is path-normalized and passed through. [StarlarkActionFactory.java](https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/analysis/starlark/StarlarkActionFactory.java)
  - A string is not registered as an input. [Bazel issue 7390](https://github.com/bazelbuild/bazel/issues/7390), via a search summary.
  - Whether a bare name resolves via PATH or execroot, sandboxed or not: UNVERIFIED.
  - The 2016 sandboxing design says PATH is reset to a hard-coded value.

**Q3. Tags and platforms.**
- `no-remote-exec`: never executed remotely, may still be cached remotely.
- `no-remote`: neither executed nor cached remotely.
- `local`: no remote execution, no remote cache, no sandbox.
- `requires-network`: network is allowed inside the sandbox.
- [common-definitions](https://bazel.build/reference/be/common-definitions)
- None of the pages read says tags affect execution-platform selection or toolchain resolution. The docs are silent on this. UNVERIFIED either way.
- Patterns that are documented:
  - exec groups with per-group `exec_compatible_with` and per-group toolchain resolution [exec-groups](https://bazel.build/extending/exec-groups);
  - the `toolchain` or `exec_group` parameter on `run`, so the action runs on the right platform [actions](https://bazel.build/versions/9.2.0/rules/lib/builtins/actions.md);
  - `@platforms//host` as the default host platform [platforms](https://bazel.build/extending/platforms).
- The Medium post (cross-platform builds) suggests aligning `--extra_execution_platforms` with the host platform. A search summary also lists `no-remote-exec` as a workaround in [discussion 20497](https://github.com/bazelbuild/bazel/discussions/20497).
- Which binary a `no-remote-exec` action gets when only a Linux remote platform is registered: no primary source read. UNVERIFIED.
- `--remote_upload_local_results` defaults to true and uploads locally executed results. `=false` makes the cache read-only for that build. [command-line](https://bazel.build/reference/command-line-reference), [caching](https://bazel.build/versions/9.2.0/remote/caching)
- `no-remote` / `no-remote-cache` actions are not cached remotely. Behavior against a server-side read-only cache: UNVERIFIED.

**Q4. Environment and credentials.**
- `--incompatible_strict_action_env` defaults to true: static PATH, no inherited `LD_LIBRARY_PATH`. `--action_env` sets variables for all actions. [command-line](https://bazel.build/reference/command-line-reference)
- `use_default_shell_env` defaults to False. When True it adds OS-dependent variables and `--action_env` values; `env` overrides them. [actions](https://bazel.build/rules/lib/builtins/actions)
- `--sandbox_default_allow_network` defaults to true ("may not work with all sandboxing implementations"). [command-line](https://bazel.build/reference/command-line-reference)
- `linux-sandbox` uses a network namespace and a read-only filesystem except the sandbox directory. `darwin-sandbox` uses `sandbox-exec`. [sandboxing](https://bazel.build/docs/sandboxing)
- Files are readable anywhere on Linux unless hidden. `$HOME` and `~/.docker` visibility were not documented; UNVERIFIED. Windows sandbox behavior: UNVERIFIED.
- `--credential_helper` default is none, timeout 10s. [command-line](https://bazel.build/reference/command-line-reference)
- The credential-helper design covers repository fetching, the remote cache and remote execution, and BES uploads. It does not say whether build actions receive credentials. [design](https://github.com/bazelbuild/proposals/blob/main/designs/2022-06-07-bazel-credential-helpers.md) So "Bazel client only" is consistent with it but not stated.
- rules_img keychain order: Bazel credential helper, `IMG_REGISTRY_AUTH_HOST`, `IMG_DOCKER_CONFIG_INLINE`, docker/podman config, ADC, ECR. `docker_config_path` in `.bazelrc` affects build-time blob downloads. [rules_img](https://github.com/bazel-contrib/rules_img)

**Q5. Launcher and runfiles.**
- The runfiles root sits next to the executable, named from the launcher path plus `.runfiles`. [rules docs](https://bazel.build/extending/rules)
- `tools` carry runfiles into the action. [actions](https://bazel.build/versions/9.2.0/rules/lib/builtins/actions.md)
- The C++ library `Runfiles::Create()` reads `RUNFILES_MANIFEST_FILE` and `RUNFILES_DIR`, falling back to argv0. `EnvVars()` gives variables for child processes. [runfiles.h](https://raw.githubusercontent.com/bazelbuild/rules_cc/main/cc/runfiles/runfiles.h)
- The Windows launcher uses `RUNFILES_DIR` if set, else `<binary>.runfiles`. [launcher.cc](https://raw.githubusercontent.com/bazelbuild/bazel/master/src/tools/launcher/launcher.cc)
  - Manifest search order: `RUNFILES_MANIFEST_FILE`, then `MANIFEST` in `RUNFILES_DIR`, then `MANIFEST` in `<binary>.runfiles`, then `<binary>.runfiles_manifest`.
  - It reads `WORKSPACE_NAME` and `SYMLINK_RUNFILES_ENABLED` from embedded launch info. Without symlinks it sets `RUNFILES_MANIFEST_ONLY=1` and `RUNFILES_MANIFEST_FILE`.
- `--enable_runfiles` is auto: off on Windows, on elsewhere. `--build_runfile_links` defaults to true. [command-line](https://bazel.build/reference/command-line-reference)

### negative:
- Not confirmed: Q2 rules_img downloader binary and bootstrap, and all of rules_oci; bare-string `executable` resolution.
- Not confirmed: tag effect on platform or toolchain selection; macOS client with only a remote Linux platform.
- Not confirmed: `$HOME` and `~/.docker` visibility in the sandbox; Windows sandbox; whether build actions receive credential-helper credentials.
- Not confirmed: tree-artifact runfiles for tools; `--nobuild_runfile_links` and remote-execution behavior for tool runfiles.
- Contradiction: `facts_version` appears in the extension docs but not in the 9.2 `module_ctx` page.
- Facts "dict of strings" vs "JSON-like" wording differs within the `module_ctx` doc.
- `reproducible=True` drops the workspace-lockfile entry but keeps hidden-lockfile caching. The docs do not state the fresh-machine rerun outright.

### leads:
- Read `SingleExtensionEvalFunction` and `BazelLockFileModule` on tag 9.2.0, not master.
- Read the rules_img source (`img_tool`) and rules_oci for the download executable.
- Bazel `SpawnStrategy` / `ExecutionPlatform` code for tag handling; `remote/rules` page for tool runfiles under remote execution.
- The runfiles design doc and `build-runfiles` for `--nobuild_runfile_links`; Bazel issue search for tree artifacts in runfiles.

Skipped: empirical runs; most pages were read through a summarizing fetch tool, so exact wording was not checked.
