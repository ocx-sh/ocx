# Research: REAPI symlinks, tool data directories, executor network, extension facts

## Metadata

**Date:** 2026-11-18
**Lane:** disputed-fact check (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** REAPI proto, Bazel 9.2.0 source and backend docs as cited; not run
**Expires:** 2027-05-18
**Caveat:** findings only, no recommendation; items marked UNVERIFIED had no primary source read.

### Findings

**Q1 Symlinks in tree artifacts**
- REAPI model: `OutputDirectory` has `tree_digest`, `is_topologically_sorted` and `root_directory_digest`. `SymlinkNode` has `target`, and the target "can be an absolute path". `ActionResult.output_symlinks` is a separate repeated field. `FileNode` has `is_executable` and `node_properties`. `NodeProperties` has `mtime` and `unix_mode`. https://raw.githubusercontent.com/bazelbuild/remote-apis/main/build/bazel/remote/execution/v2/remote_execution.proto
- `SymlinkAbsolutePathStrategy` has three values. `DISALLOWED`: the server returns INVALID_ARGUMENT on absolute input symlinks and FAILED_PRECONDITION on absolute output symlinks. `ALLOWED`: targets may escape the input root. The capability is `CacheCapabilities.symlink_absolute_path_strategy`. Same proto URL.
- Upload side, from `UploadManifest.java` at 9.2.0 (https://raw.githubusercontent.com/bazelbuild/bazel/9.2.0/src/main/java/com/google/devtools/build/lib/remote/UploadManifest.java):
  - Inside a tree artifact, relative and dangling symlinks are preserved as `SymlinkNode`.
  - A non-dangling absolute symlink is followed and uploaded as the file or directory it points to ("for historical reasons").
  - An absolute or dangling absolute symlink is allowed only if the server advertises `ALLOWED`. Otherwise Bazel throws "Spawn output %s is an absolute symbolic link to %s, which is not allowed by the remote cache".
  - A looping symlink counts as dangling. Special files are rejected.
  - Plain output files behave the same way: relative symlinks (file or directory) are preserved, non-dangling absolute ones are followed.
- Download side, from `RemoteExecutionService.java` at 9.2.0: symlinks in `Tree` directories are recreated locally after all downloads finish ("dangling symlinks might not be supported on all platforms"). A path collision with different targets throws. https://raw.githubusercontent.com/bazelbuild/bazel/9.2.0/src/main/java/com/google/devtools/build/lib/remote/RemoteExecutionService.java
- Issue [#16361](https://github.com/bazelbuild/bazel/issues/16361) was closed "not planned" on 2023-12-04. The closing comment says remotely produced tree artifacts now handle symlinks "the same way" as locally produced ones. The issue says Bazel dereferences relative symlinks in tree artifacts locally, and that Google's RBE always dereferences despite the spec. The tree artifact definition is "a collection of regular files". Tracking issue [#15454](https://github.com/bazelbuild/bazel/issues/15454) for unresolved symlinks in trees stays open.
- As an action input, `SpawnInputExpander.addInputs` expands a tree into per-file `TreeFileArtifact` entries. The code has no symlink-specific handling. https://raw.githubusercontent.com/bazelbuild/bazel/9.2.0/src/main/java/com/google/devtools/build/lib/exec/SpawnInputExpander.java
  - Whether symlinks survive as symlinks in the input Merkle tree is UNVERIFIED; it depends on `TreeArtifactValue`, which I did not read.
- Flag defaults in 9.2.0 are UNVERIFIED. The 9.2.0 `RemoteOptions.java` fetch returned only `--remote_default_exec_properties` (default null). Older evidence only: `--incompatible_remote_symlinks` was flipped to true at some point after 0.22, and `--incompatible_remote_dangling_symlinks` was introduced as true in 2022, per [commit 5572695](https://bazel.googlesource.com/bazel/+/5572695341483bd4ea5f9c311061b7317c0249a8) and a web-search summary of the 2022 commit. I could not confirm the flags still exist in 9.2.0.
- `--experimental_allow_unresolved_symlinks`: `declare_symlink` and `symlink` are gated by it (issue [#10298](https://github.com/bazelbuild/bazel/issues/10298)). A search result said it does not work for tree artifact children. I did not read the 9.2.0 source for this.
- Hardlinks, modes other than the exec bit, and `node_properties` handling in Bazel are UNVERIFIED. The proto defines `unix_mode` and `mtime`, and `supported_node_properties` is an `ExecutionCapabilities` field.
- Backend symlink differences (BuildBuddy, EngFlow, Buildbarn, Buildfarm, bazel-remote) are UNVERIFIED. A Buildbarn symlink repro exists at https://github.com/castler/buildbarn_bazel_symlink_issue_repro (not read).

**Q2 Tool data directory**
- Runfiles inputs: for a runfiles tree, `addSingleRunfilesTreeToInputs` adds each tree child as a separate input at `runfiles_location/<parent_relative_path>`. An empty tree becomes an empty directory. Archived-tree mode is a special case. Source: SpawnInputExpander URL above. This covers (a) in expanded form only. Whether it applies identically in local sandbox and remote runs, and whether `tools=` pulls runfiles, are UNVERIFIED.
- (b) The Windows or manifest-only representation of a directory is UNVERIFIED. I did not read it. By the code above, children are listed as files, not as a directory entry.
- (c) `LocalSpawnRunner.java` at 9.2.0 does not mention `RUNFILES_DIR` or `RUNFILES_MANIFEST_FILE` (the fetch tool answered "No"). `SpawnRunner.java` has no such env either. I read only these two files, so a setter elsewhere is not excluded.
- Skylib `run_binary` passes the tool via `executable` and `tools`, and env as `default_shell_env` plus the `env` attribute, with `use_default_shell_env=False`. It does not set `RUNFILES_*`. https://raw.githubusercontent.com/bazelbuild/bazel-skylib/main/rules/run_binary.bzl
- rules_python issue [#3518](https://github.com/bazel-contrib/rules_python/issues/3518) (fixed in PR #3644, closed 2026-04-03): a py_binary bootstrap sets `RUNFILES_DIR` itself, and a nested binary inherits the wrong value. Its bootstrap `_find_runfiles_root()` rejects a bad value and falls back to `__file__`.
- (d) Details for rules_js, rules_go and Bazel's Windows `launcher.exe` are UNVERIFIED.

**Q3 Network**
- Bazel docs [remote/rules](https://bazel.build/remote/rules) do not mention `requires-network`.
- Whether Bazel forwards `requires-network` to remote execution is UNVERIFIED. Bazel source was not read. A search result from Aspect docs says it applies to the sandbox. https://docs.aspect.build/cli/help/tags
- BuildBuddy: the `network` property, set via `exec_properties` on a platform or a target, takes `off`, `external` or `host`. `dockerNetwork` is deprecated and takes `off` or `bridge`. Defaults: `off` for oci, podman, docker and sandbox; `external` for firecracker. https://buildbuddy.io/docs/rbe-platforms
- EngFlow: `"dockerNetwork": "standard"` in platform `exec_properties`. Default is not stated. https://docs.engflow.com/re/client/bazel-first-time.html
- Namespace: `namespace_requires_network`, set per action. Snippet only. https://namespace.so/docs/bazel/execution
- Buildbarn and Buildfarm network controls are UNVERIFIED. The Buildfarm docs URL returned 404.
- `--remote_default_exec_properties`: "if an execution platform does not already set exec_properties". The Bitrise and BuildBuddy docs say the same, so it is dropped when a platform or target sets its own. https://bazel.build/reference/command-line-reference
- Rule-author `exec_properties` and `exec_group` mechanics were not read, so UNVERIFIED beyond BuildBuddy showing `exec_properties` on a target.

**Q4 Facts**
- Introduced in 8.5.0rc2 (2025-11-14), per release notes: https://newreleases.io/project/github/bazelbuild/bazel/release/8.5.0rc2. 9.2.0rc1 added `module_extension(facts_version=...)`: https://newreleases.io/project/github/bazelbuild/bazel/release/9.2.0rc1. The 9.2.0 docs show `module_ctx.facts` with no experimental label: https://bazel.build/rules/lib/builtins/module_ctx. Aspect post: https://aspect.build/blog/bazel-facts-api.md.
- `SingleExtensionEvalFunction.java` at 9.2.0:
  - `lockfileFacts` is taken from the workspace lockfile when `facts_version` matches. Otherwise it is taken from the hidden lockfile (only if the workspace lockfile has no entry), else empty.
  - `lockfileFacts` is passed as the last argument to `extension.run(...)`. How `module_ctx.facts` is built from it was not shown.
  - Lockfile reuse needs all of: matching transitive digest, matching usages digest, and no recorded-input diffs.
  - In ERROR mode, a locked entry that is not reproducible throws. A post-run check requires the new facts to equal the workspace facts. Hidden-lockfile facts are excluded from that check.
  - A code comment says there is "intentionally no diff check for facts".
- `BazelLockFileModule.java` at 9.2.0:
  - `MODULE.bazel.lock` receives only non-reproducible extension entries. Reproducible ones go to the hidden lockfile in the output base.
  - Facts, however, are merged (old plus newly evaluated), filtered to extensions still used and non-empty, and set on both the workspace and hidden lockfiles.
  - Facts versions are stored only when non-zero.
- Inference for a reproducible extension with non-empty facts on a fresh machine with a committed lock:
  - The extension's entry is not in the workspace lock and the hidden lock is empty, so the implementation re-runs.
  - It receives the committed facts via `lockfileFacts`, provided `facts_version` matches.
  - Facts are written to the workspace `MODULE.bazel.lock`, though the extension entry itself is not.
  - This chains the two files' behaviours. No single passage states it end to end, and I did not run it.

### negative:
- No primary-source confirmation of 9.2.0 flag defaults for the three symlink flags.
- Nothing found on Bazel setting `RUNFILES_*` for actions. `LocalSpawnRunner.java` and `SpawnRunner.java` do not set it.
- Buildfarm docs URL 404 (https://bazelbuild.github.io/bazel-buildfarm/docs/execution/execution_properties/).
- `/home/mherwig/dev` grep for `incompatible_remote_symlinks` returned nothing.

### leads:
- `RemoteOptions.java` and `RemoteSpawnRunner` at 9.2.0, searching the options by name.
- `TreeArtifactValue` and `ActionInputMapSink` for symlink handling in tree inputs.
- Bazel `ExecutionRequirements` (`requires-network`) and `StandaloneSpawnStrategy` for remote forwarding.
- rules_js `js_binary` launcher and `bazel-lib` runfiles docs.
- Bazel `launcher.cc` for the Windows launcher.
- Buildbarn bb-remote-execution and bb-storage docs on symlinks and `SymlinkAbsolutePathStrategy`.
- Issue [#15454](https://github.com/bazelbuild/bazel/issues/15454).

Not checked: Q2 (b) and (d), most backend differences, and 9.2.0 flag defaults.
