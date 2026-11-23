# Research: Bazel RBE-compatible delivery of prebuilt tool packages

## Metadata

**Date:** 2026-11-17
**Lane:** prior-art web scan (`/hex-discuss` entry wave, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** web sources as cited; several marked UNVERIFIED by the lane
**Expires:** 2027-05-17
**Caveat:** findings only, no recommendation; items marked UNVERIFIED had no primary source read.
(Date 2026-11-17. Many fetches were truncated search summaries; items marked UNVERIFIED lack a primary source read.)

### Direct answer
Neutral summary of established facts per shape:
- **Shape (i), repo-rule extraction.** Repo rules run on the Bazel client host, at fetch time. Output is reusable remotely only through the experimental remote repo contents cache, and only if the rule is declared reproducible. Bazel's own RBE guide says host-built artifacts may not run on the execution platform. It says only platform inspection and dependency enumeration belong in repo rules, and target-specific artifacts belong in build actions. Constraints in the 9.2 cache: no cross-repo symlinks and no symlinks into the main repo (axis 2). Strength: no extractor tool is needed on the executor.
- **Shape (ii), blob download plus extraction action.** Extraction output enters the normal action cache and CAS, so it is remote-cacheable and executable on executors. Costs: a tree artifact, or an input tree of many small files; an exec-platform extractor (tar/zstd) must be a toolchain; and the file list is unknown at analysis time.
- **Cross-platform.** Per-platform repos plus toolchain resolution is the established idiom. Repo-rule-installed host tools "don't respect toolchain resolution" ([platforms docs](https://bazel.google.cn/versions/8.7.0/extending/platforms)).

### Trends
- **Trending:** metadata-first/lazy blob handling ([rules_img](https://tweag.io/blog/2025-12-18-rules_img/), Dec 2025). Reproducible repo rules plus the remote repo contents cache (9.x).
- **Established:** per-platform repos with `toolchain()` and `exec_compatible_with`. Extraction in actions (rules_js lifecycle hooks). Remote Asset API downloader (Buildbarn, EngFlow).
- **Emerging:** `repo_metadata(reproducible=True)`. rules_python adopting it ([1.9.0 docs](https://rules-python.readthedocs.io/en/stable/api/rules_python/python/private/pypi/whl_library.html)). `--rewind_lost_inputs` on by default in 9.3.0.
- **Declining:** WORKSPACE (removed in 9.0). Host-installed-tool repo rules, e.g. rules_nixpkgs without a shared store.

### Key findings
1. **Repo rules and RBE.**
   - Repo rules run on the client host in the loading phase. The Bazel RBE guide says to avoid compiling or platform-mutating in repo rules and to use toolchains, not `PATH` ([remote/rules](https://bazel.google.cn/remote/rules?hl=en)). Host/exec/target definitions are in [platforms](https://bazel.google.cn/versions/8.7.0/extending/platforms).
   - A prebuilt tool fetched for the host platform is wrong for an executor when macOS client ≠ Linux executor. EngFlow notes the host OS must match the exec OS for host-built tools ([known issues](https://docs.engflow.com/re/client/bazel-known-issues.html), 2020–21 notes).
   - rules_nixpkgs is the canonical failure. Nix paths are installed on the client, but executors lack them ([tweag, Feb 2024](https://tweag.io/blog/2024-02-29-remote-execution-rules-nixpkgs), [issue #180](https://www.github.com/tweag/rules_nixpkgs/issues/180)).
   - toolchains_llvm, rules_multitool and bazel-lib hub-repo mechanics: UNVERIFIED (no primary source read).

2. **Cache tiers.**
   - The local `--repo_contents_cache` and the experimental `--experimental_remote_repo_contents_cache` startup flag arrived in Bazel 9.0 ([blog, Jan 2026](https://blog.bazel.build/2026/01/20/bazel-9.html)).
   - `repository_ctx.repo_metadata(reproducible=True)` declares that the same inputs give the same output, even when untracked conditions change. It allows caching across workspaces but does not guarantee an entry. Local repo rules are never cached ([docs](https://bazel.build/rules/lib/builtins/repository_ctx#repo_metadata)). The `repo_metadata` page exists under Bazel 8.3.1 and 8.6.0 docs, so it is not 9-only.
   - 9.0.2/9.1: keys include host OS and CPU ([#29195](https://newreleases.io/project/github/bazelbuild/bazel/release/9.1.0)). The cache now supports all reproducible rules (#29225).
   - 9.2.0: chains of symlinks are materialised as action inputs. No symlinks into the main repo. No caching of repos with cross-repo symlinks (#30190, pending #30160). A non-gRPC backend fix and a crash fix ([notes](https://newreleases.io/project/github/bazelbuild/bazel/release/9.2.0)).
   - The flag can hang after a remote cache read timeout before 9.3.0. EngFlow says to enable it only on 9.3.0+ ([known issues](https://docs.engflow.com/re/client/bazel-known-issues.html)). EngFlow also reports it is not recommended for production as of Aug 2026.
   - The recovery PR for cached repos with evicted CAS blobs, [#30226](https://github.com/bazelbuild/bazel/pull/30226), was CLOSED on 2026-08-13. Whether it merged is unclear. The 9.3.0 notes do not list it.
   - Repository cache keys on sha256 (and `canonical_id`). Distdir and vendor details: UNVERIFIED, since bazel.build/external/cache returned 404.
   - Lazy or materialise-on-demand external repos: no primary source found.
   - `--rewind_lost_inputs` is default-on in 9.3.0 (#31416).

3. **BwoB.**
   - Bazel 7 changed the default to `toplevel`. `minimal` does not download outputs unless a local action needs them ([BwoB in 7](https://blog.bazel.build/2023/10/06/bwob-in-bazel-7.html)).
   - Old rules_js docs warned of tree-artifact plus sandbox problems under toplevel/minimal on Bazel 6.0 rc1/rc2 (historical).
   - 9.3.0 fixes redownload of deleted top-level outputs (#30963) and `--rewind_lost_inputs` ([notes](https://newreleases.io/project/github/bazelbuild/bazel/release/9.3.0)).
   - 9.2.0: a lost input in a tree artifact shared between actions was unrecoverable by rewinding under `--experimental_output_paths=strip` (#30193).
   - Direct docs on repo outputs under BwoB: UNVERIFIED.

4. **Shape (ii) in the wild.**
   - **rules_js:** an npm_import per package via the downloader, a virtual store built from tree artifacts. Lifecycle hooks run as actions, so results go in the remote cache. Individual packages can be tagged `no-remote-exec` ([docs](https://docs.aspect.build/bazel/javascript/pnpm_rules_js)).
   - **rules_oci:** the pull repo rule downloads all blobs into a tree artifact. Layers are tar files, and manifest assembly takes whole layer tars as inputs. With RBE the data flows registry → client → cache ([tweag](https://tweag.io/blog/2025-12-18-rules_img/)). Pulls are cached via the repository cache when a digest is given ([migration guide](https://docs.aspect.build/guides/rules_oci_migration)).
   - **rules_img:** the pull rule fetches about 10 KB of manifest and config. Layer actions write to CAS and emit descriptors. A `lazy` layer option downloads in actions. A compact layer storage mode never writes layer tars to the remote cache.
   - **rules_python:** extraction in the repo rule (`whl_library`). It is marked reproducible when extraction is pure Starlark. A sibling repo supplies the original wheel lazily. [PR #3430](https://github.com/bazel-contrib/rules_python/pull/3430) moves to `repository_ctx.extract`.
   - rules_distroless, rules_apko, rules_multitool, aspect `tar`: UNVERIFIED. The rules_distroless search found no rationale.

5. **Tree artifacts under REAPI.**
   - Tree artifacts were originally regular files only. A flag disallowed symlinks in remote tree artifacts, and issue [#16361](https://github.com/bazelbuild/bazel/issues/16361) was closed after remote handling was unified with local. Dangling symlinks need `--incompatible_remote_dangling_symlinks` ([commit](https://bazel.googlesource.com/bazel/+/5b46a48db8b380ba9afe2df2cb7d564b0c927913)). Absolute symlinks are gated on a server capability (UNVERIFIED which one).
   - Open issue [#28953](https://www.goodfirstissue.org/bazelbuild/bazel/issues/28953) (Mar 2026) reports dangling symlinks in custom rule outputs.
   - A Buildbarn repro shows a relative symlink becoming a plain file remotely ([repro](https://github.com/castler/buildbarn_bazel_symlink_issue_repro), date unknown).
   - 9.3.0: fixes incremental remote builds failing under stale dangling symlinks (#30969).
   - Merkle-tree cost and file-count numbers: no primary source found. Archive-as-input and path mapping (`--experimental_output_paths=strip`) are mitigations, but I read only the one 9.2.0 bug note on the latter.

6. **Launchers.**
   - Windows has no symlink forest by default, only a runfiles manifest, and uses `rlocation` ([Bazel commit](https://bazel.googlesource.com/bazel/+/a148b4c25a7e343eac6a9a972f482711f8bef944)).
   - On Linux and macOS, `RUNFILES_DIR` is faster than the manifest.
   - macOS codesign inside actions versus at fetch time: no source found.

7. **Auth and downloader.**
   - [#29660](https://github.com/bazelbuild/bazel/issues/29660): open, P3, "help wanted". The downloader forwards explicit `Authorization` headers across cross-origin redirects and on HTTPS→HTTP downgrade. Copybara commit 4f13ff7 on 2026-07-27 is linked, but the issue is still open. Related: #30013 (netrc/auth dropped on same-host redirects). This affects OCI blob redirects (registry → CDN).
   - `--downloader_config` rewrites, allows or blocks URLs. It is not the remote-asset layer. 9.3.0 fixes a `download_and_extract` failure when it blocks all URLs and the repository cache serves the archive (#30470).
   - Remote downloader: Bazel uses only the `FetchBlob` side of the Remote Asset API, and few servers implement it (bb-remote-asset, bazel-remote "very experimental"). Flag names: `--experimental_remote_downloader` in older docs, `--remote_downloader` on EngFlow. `--remote_downloader_propagate_credentials` plus `--credential_helper` passes helper credentials to the asset service ([EngFlow](https://docs.engflow.com/re/client/remote-asset-api.html), [Buildbarn](https://github.com/buildbarn/bb-remote-asset)).
   - Whether it routes `repository_ctx.download` blobs, and its fallback behaviour: UNVERIFIED. Bearer-token exchange with Bazel's downloader: UNVERIFIED.

8. **Determinism.**
   - Reproducible rules are cached by key, and the cache now includes host OS/arch.
   - rules_python marks a rule reproducible only for pure-Starlark extraction.
   - Mode bits, symlink targets, manifest ordering and absolute paths: no primary source read on how rulesets guarantee hits.

### Sources
- https://blog.bazel.build/2026/01/20/bazel-9.html — 9.0 repo contents cache
- https://bazel.build/rules/lib/builtins/repository_ctx#repo_metadata — reproducible semantics
- https://newreleases.io/project/github/bazelbuild/bazel/release/9.1.0 — 9.1 notes
- https://newreleases.io/project/github/bazelbuild/bazel/release/9.2.0 — 9.2 notes
- https://newreleases.io/project/github/bazelbuild/bazel/release/9.3.0 — 9.3 notes
- https://github.com/bazelbuild/bazel/pull/30226 — eviction recovery PR (closed)
- https://docs.engflow.com/re/client/bazel-known-issues.html — hang before 9.3.0; EngFlow not recommending the flag
- https://github.com/bazelbuild/bazel/issues/29660 — downloader redirect headers
- https://bazel.google.cn/remote/rules?hl=en — RBE rules adaptation
- https://bazel.google.cn/versions/8.7.0/extending/platforms — platforms and toolchains
- https://tweag.io/blog/2025-12-18-rules_img/ — rules_oci vs rules_img
- https://tweag.io/blog/2024-02-29-remote-execution-rules-nixpkgs — nixpkgs RBE (older than 18 months)
- https://www.github.com/tweag/rules_nixpkgs/issues/180 — nixpkgs RBE issue
- https://docs.aspect.build/bazel/javascript/pnpm_rules_js — rules_js
- https://rules-python.readthedocs.io/en/stable/api/rules_python/python/private/pypi/whl_library.html — wheel handling
- https://docs.engflow.com/re/client/remote-asset-api.html — remote downloader
- https://bazel.googlesource.com/bazel/+/5b46a48db8b380ba9afe2df2cb7d564b0c927913 — dangling symlinks flag (older)
- https://github.com/bazelbuild/bazel/issues/16361 — tree-artifact symlinks (older)
- https://bazel.googlesource.com/bazel/+/a148b4c25a7e343eac6a9a972f482711f8bef944 — Windows runfiles manifest (old)

### negative:
- bazel.build/external/cache returned 404, so the repository cache, distdir and vendor claims are unverified.
- No source states that remote repo contents cache is stable. EngFlow advises against it.
- I could not confirm whether #30226 merged.
- No source read on lazy materialised external repos, the Merkle tree cost of node_modules, macOS codesign, or toolchains_llvm and multitool hub design.
- The discussion thread #27509 returned HTTP 429.
- Some sources are old: 2019–2023 BwoB and symlink material, the 2024 nixpkgs post, and the 2020–21 EngFlow notes.

### leads:
- Read the rules_js, bazel-lib and rules_multitool source for the per-platform hub plus `toolchain()` pattern and tar-in-action.
- Check the bazel-remote or Buildbarn docs on whether `FetchBlob` takes OCI blob URLs with auth headers.
- Read the Bazel source for the remote repo contents cache (file-per-CAS-entry, symlink model) and the #30160 tracker.
- Look at rules_img `layer_handling` and its CAS-descriptor design as a model for sha256-addressed blob packages.
- Check the REAPI spec on `OutputSymlink`, `SymlinkAbsolutePathStrategy` and `node_properties` (mode bits).
