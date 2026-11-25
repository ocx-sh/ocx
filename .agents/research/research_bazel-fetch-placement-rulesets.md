# Research: Bazel fetch placement — how other rulesets move bytes

## Metadata

**Date:** 2026-11-23
**Lane:** competitive/vendor (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** Ruleset and Bazel facts below were read as SOURCE FILES from shallow `git clone`s of the default branch (commits listed per section; Bazel from `main` 2a2fea0 and tag 9.3.3 b592da4f), plus `gh` read-only calls for issues, PRs and release notes (JSON bodies, read verbatim). Read through a summarising WebFetch only: the Tweag rules_img article (2025-12-18) and one Aspect page. The Aspect page was also fetched raw with curl and grepped. The Kleaf rc files were read raw from android.googlesource.com (base64 text). Nothing was run: no ruleset was built, no REAPI backend exercised. Statements that are inference from code are labelled INFERENCE; anything with no primary source is UNVERIFIED.
**Expires:** 2027-05-23
**Caveat:** findings only, no recommendation

Scope vocabulary: "repo rule" = repository rule / module extension run on the Bazel client at fetch time; "action" = build action, which can run on a REAPI executor.

## 0. Primary-source answers

### 0.1 Does `requires-network` change remote caching or remote execution?

No. In Bazel source, `ExecutionRequirements.REQUIRES_NETWORK = "requires-network"` is documented as "Enables networking for a spawn if possible (only if sandboxing is enabled and if the sandbox supports it)" (`src/main/java/com/google/devtools/build/lib/actions/ExecutionRequirements.java`, main 2a2fea0). Its only readers are `Spawns.requiresNetwork(...)` in `src/main/java/com/google/devtools/build/lib/actions/Spawns.java`. That method is called only from the local sandbox runners: `LinuxSandboxedSpawnRunner.java:311`, `DarwinSandboxedSpawnRunner.java:225` and `DockerSandboxedSpawnRunner.java:232`. A grep of `src/main/java/com/google/devtools/build/lib/remote/` finds no reference to it. The docs say the same: "This tag only has an effect if sandboxing is enabled" (bazel.build 9.2.0 `reference/be/common-definitions`, i.e. `docs/versions/9.2.0/reference/be/common-definitions.mdx` in the repo).

Tags that do change caching or execution, from `Spawns.java` `mayBeCached`, `mayBeCachedRemotely`, `mayBeExecutedRemotely`:

| Tag | Effect |
|---|---|
| `no-cache` | no local or remote caching (disk cache counts as local) |
| `local` | not cached, not remotely executed, not sandboxed (deprecated spelling per the source comment) |
| `no-remote-cache` | not cached remotely; may still execute remotely |
| `no-remote-cache-upload` | no upload; remote exec still allowed |
| `no-remote-exec` | not executed remotely; may still be cached remotely |
| `no-remote` | both of the two above |
| `no-sandbox` | no local sandbox; still cacheable and remotely executable |
| `requires-network` / `block-network` | local sandbox network only |

Consequence (INFERENCE from the code above): a Starlark action tagged only `requires-network` stays remotely cacheable and remotely executable. Whether the executor has egress is a property of the REAPI backend, not of this tag. Backend semantics (BuildBuddy, EngFlow, Buildbarn) were not read.

### 0.2 Remote repo contents cache

- Flag: startup option `--experimental_remote_repo_contents_cache` (`src/main/java/com/google/devtools/build/lib/remote/options/RemoteStartupOptions.java`). Default false. Help text: stores the results of reproducible repository rules in the remote cache; on a hit the repo contents are kept "in an in-memory file system and are only downloaded when needed, either by Bazel itself or an action that runs locally". The local sibling is `--repo_contents_cache` (RepositoryOptions.java).
- Release: the flag is absent from 8.7.0, 8.6.0, 8.5.0 and 8.4.0 (checked with `git grep` on each tag). It is present in 8.8.0 (published 2026-08-31, release notes list it) and 9.0.0 (published 2026-01-20, release notes list it). So the first release carrying it is 9.0.0 (2026-01-20), and it was later backported to 8.8.0. The local `--repo_contents_cache` is in 9.0.0 notes as new. A claim "9.0 only" would be wrong for the 8.x line.
- Status in 9.x: still experimental. The name keeps the `experimental_` prefix on 9.3.3. The tracking issue [bazelbuild/bazel#31456](https://github.com/bazelbuild/bazel/issues/31456) "Stabilize `--experimental_remote_repo_contents_cache`" (opened 2026-10-02, OPEN) lists as unfinished: warn when a repo is silently left out because of symlinks; output-service support; `--remote_download_symlink_template`; rewinding with non-build commands; "Decide whether `bazel fetch` should materialize repos"; Skyframe dependencies of obsolete cache alternatives; keying on `native.bazel_version`.
- Hang and correctness history (all from release notes or issues read via `gh`):
  - [#30780](https://github.com/bazelbuild/bazel/issues/30780) (opened 2026-08-18, closed 2026-09-08): 9.2.0 hangs indefinitely after a remote-cache ByteStream read times out while loading a file through the remote repo contents cache. Closed with a comment citing fixes in PRs #30819 and #30791 and a "fork 9.3.0" label. 9.3.0 was published 2026-10-07. The 9.3.0 release notes do not name these two PRs (grep), so the fix release is taken from the issue comments, not the notes. EngFlow's known-issues page (read in an earlier lane) says enable only on 9.3.0+.
  - 9.0.2 and 9.1.0 notes: cache keys now include host OS and CPU (#29194, #29195). 9.1.0: the remote cache now supports all reproducible repo rules (#29225).
  - 9.2.0 notes: chains of symlinks materialise as action inputs, symlinks into the main repo are no longer supported (#29959); "no longer caches or restores repos with cross-repo symlinks" (#30190, re-enable tracked in #30160); a crash fix when no remote or disk cache exists (#30060); a "No such file" fix for a symlink to a `.bzl` file (#30037).
  - 8.8.0 notes: non-gRPC backend fix (#30029); .bzl prefetch (#29082).
  - Open as of 2026-11-23: [#31969](https://github.com/bazelbuild/bazel/issues/31969) crash ("cannot relativize") when a remote-exec input from this cache is missing from the CAS before remote execution (reported on 9.3.0); [#32346](https://github.com/bazelbuild/bazel/issues/32346) dangling symlink between repos after re-fetch; [#32820](https://github.com/bazelbuild/bazel/issues/32820) incompatible with `--remote_local_fallback`; [#32601](https://github.com/bazelbuild/bazel/issues/32601) backslash in a file name; [#31005](https://github.com/bazelbuild/bazel/issues/31005) materialises entire repos to local disk; [#32179](https://github.com/bazelbuild/bazel/issues/32179).
  - [PR #30226](https://github.com/bazelbuild/bazel/pull/30226) "Recover from lost CAS blobs during full repo materialization" is state CLOSED (not merged per the state field; merged-ness not checked further). 9.3.0 notes: `--rewind_lost_inputs` default on (#31416).
- Eligibility (from `RepositoryFetchFunction.java` lines ~264-360 and `RepositoryUtils.java` `replantSymlinks`, tag 9.3.3):
  1. The rule's result must be `Reproducibility.YES`: the implementation returns `rctx.repo_metadata(reproducible = True)` (API exists in Bazel 8.3+ per the many rulesets that guard it with `hasattr(rctx, "repo_metadata")` and a "Bazel <8.3.0" comment).
  2. The rule must not be `local = True`.
  3. The repo must be "safe for remote cache" after symlink replanting: every symlink must resolve inside the repo (`symlinksResolveWithinRepo`; `TODO(#30160)` in source). A repo with a cross-repo symlink is silently not cached.
  4. A rule that records inputs at runtime (`getenv`, `watch`) is cached as a DAG of action-cache entries keyed by the recorded values (`RemoteRepoContentsCacheImpl.java` class comment). The 8.8.0 and 9.0.0 release notes state the narrower form: "reproducible repository rules without dependencies added at runtime (e.g., via `repository_ctx.watch` or `.getenv`)"; 9.1.0 widened it (#29225).
  5. Upload only happens when local results are uploaded (`uploadLocalResults` gates `CachePolicy.REMOTE_CACHE_ONLY` in `RemoteRepoContentsCacheImpl.java`; the flag name behind it was not traced). The backend needs the REAPI action cache and CAS only; no Remote Asset service.
- Entries live in the AC and CAS, so they are subject to the backend's eviction policy (`RemoteRepoContentsCacheImpl.java` comment: "regular LRU eviction policies remain effective").

### 0.3 `--experimental_repo_remote_exec`

Starlark-semantics flag, default false, metadata tag EXPERIMENTAL, help "If set to true, repository_rule gains some remote execution capabilities" (`BuildLanguageOptions.java:249`). When set, `repository_rule(remotable = True)` rules get an extra string-dict attribute `exec_properties` (`StarlarkRepositoryModule.java:76-79`) and `repository_ctx.execute(...)` runs on the remote executor if a remote executor is configured (`StarlarkBaseExternalContext.canExecuteRemote()`: flag enabled AND rule remotable AND `remoteExecutor != null`). In remotable mode `execute` arguments must be strings or labels, not paths (`validateExecuteArguments`). Only `execute` is remoted; `download` and `extract` are not touched by this flag (INFERENCE from the code read). Documented in the 8.0.0 `repository_rule` docs as "Experimental ... may change at any time. Please do not depend on it." Status still experimental on main. A 2025-08-06 comment on bazelbuild/bazel#6359 (from the community lane) says it is offered "without vouching for its quality". No ruleset read in this lane sets `remotable = True` (not exhaustively searched).

### 0.4 Remote Asset API flag (context for the placements below)

`--experimental_remote_downloader=<grpc endpoint>` ("A Remote Asset API endpoint URI, to be used as a remote download proxy") and `--experimental_remote_downloader_local_fallback` ("Whether to fall back to the local downloader if remote downloader fails") (`RemoteOptions.java:146-163`). It applies to the Bazel downloader, so to `rctx.download*` and `http_archive`, not to downloads inside actions.

## 1. rules_img (Tweag, bazel-contrib/rules_img)

Read: `fc9310f` (2026-11-16, "CI: enable BwoB and remote repo contents cache (#886)"; latest tag seen v0.3.9). Files: `img/private/repository_rules/pull.bzl`, `pull_blob.bzl`, `download.bzl`, `img/private/download_blobs.bzl`, `img/private/common/deploy_attrs.bzl`, `docs/pull.md`, `docs/push-strategies.md`, `docs/remote-cache.md`, `docs/authenticating-build-actions.md`, `.github/workflows/.bazelrc.buildbuddy`. Article: [Tweag, 2025-12-18](https://tweag.io/blog/2025-12-18-rules_img/) (summarising fetch).

Mechanism, `pull(layer_handling = ...)` (attribute doc, `pull.bzl`):
- `shallow` (default): the repo rule fetches only manifest and config. Layer bytes are "fetched only if needed during push operations, but [are] not available during the build." The article: "The layer blob stays in Bazel's CAS, while only a small JSON descriptor [moves]"; "The actual blob bytes only move later during `bazel run` when you push or load." It does not say how base-layer blobs got into the CAS in that picture; the push tools read blobs from the remote cache by REAPI ByteStream/Batch calls (`docs/remote-cache.md`) and fall through to a registry on `NOT_FOUND`.
- `eager`: the repo rule downloads every layer, "inefficient as all layers are downloaded regardless of whether they're needed". Bytes then reach actions as repo-file inputs uploaded from the client.
- `lazy` (marked EXPERIMENTAL in the attribute doc): the repo rule generates a `download_blobs` target per image (`tags = ["requires-network"]`). `img/private/download_blobs.bzl` runs one `ctx.actions.run` per blob: mnemonic `DownloadBlob`, executable = the `img` tool from the img toolchain, `execution_requirements = {"supports-path-mapping": "1"}`, no `no-cache`, no `no-remote*`, no `requires-network` on the action itself (the tag sits on the rule target; whether a rule-level `tags` entry reaches a Starlark action's execution info was not checked: Bazel's docs say tags on non-test targets are used "to control sandboxed execution of genrules and Starlark actions", but the code path was not read). The action downloads by digest from the registry and the digest is verified by the tool (`--digest`). It is therefore a cacheable action: a hit means the registry is not contacted. Env: `IMG_CREDENTIAL_HELPER*` is cleared "so a host-local helper path is never baked into a potentially-remote action"; auth either via `REGISTRY_AUTH_FILE` path or a `config` label that becomes an action input (doc warns it "may be uploaded to a remote cache or remote executor. Do not put secrets" in it).
- Metadata fetch (`rctx.download` with sha256, or `rctx.execute` of the `img` tool; attribute `downloader = "img_tool" | "bazel"`). `pull_blob.bzl` returns `rctx.repo_metadata(reproducible = True)` when a digest is given, "allows participating in repo contents cache".
- Push-time bytes: `docs/push-strategies.md` has `eager`, `lazy` (stream missing blobs from Bazel's remote cache to the registry), `cas_registry`, `bes`, plus "push at build time" actions whose default `execution_requirements` are `{"requires-network": "1", "supports-path-mapping": "1"}` (`deploy_attrs.bzl:397-407`; "Setting it to `{}` removes the `requires-network` marker"; the doc says to override "for example to route the actions to a specific remote execution pool"). The README says build-time push uploads "straight from the remote execution cluster to the registry; bytes never touch the machine running Bazel".

What it requires of the backend, per `docs/push-strategies.md`:
- shallow/eager: nothing beyond a registry.
- lazy push: "Requires a Bazel remote cache", `--digest_function=sha256`, and "Push fails if required blobs are evicted from the CAS before the push runs". Uses ByteStream and Batch REAPI calls, with retry on `UNAVAILABLE`/`RESOURCE_EXHAUSTED` etc. and `NOT_FOUND` treated as a miss (`docs/remote-cache.md`). A BuildBuddy-specific path exists for build-action auth ("This mechanism requires BuildBuddy remote execution", `docs/authenticating-build-actions.md`; an OCI distribution gateway option is described for executors).
- `layer_handling = lazy` and build-time push: executor egress to the registry. Doc: "Some sandboxing setups deliberately block network access from actions ... build-time pull/push will not work in those environments." Under RE "the connection is opened by the executor, not by the machine running Bazel."
- No use of the Remote Asset API for layers was found: grep for `remote_asset`/`Remote Asset` in the tree returns nothing, and the article says it is not mentioned. rules_img's own CI sets `--experimental_remote_downloader=grpcs://...buildbuddy.io` with `--experimental_remote_downloader_local_fallback` and (since #886) `--remote_download_minimal` and `startup --experimental_remote_repo_contents_cache` (`.github/workflows/.bazelrc.buildbuddy`); the CI strips the startup line for nested Bazel 7/8 runs because they "reject the Bazel 9-only --experimental_remote_repo_contents_cache" (`ci.yml`; note 8.8.0 also has it, so the comment is about 7.x and 8.0-8.7).

Without the feature: on a backend with no remote cache, `lazy` push is unavailable and `eager` push "works with any standard container registry". On an executor without egress, `layer_handling = lazy` and build-time push fail (per the doc above); `shallow` and `eager` are unaffected.

`bazel fetch` / `bazel vendor`: not mentioned in README, docs or the article (grep). INFERENCE: `bazel fetch` covers manifest, config and (for `eager`) layers, because those are repo-rule outputs; `lazy` layers are action outputs and are not covered. UNVERIFIED by a run.

Stated reasoning (article and README): rules_oci's repository rule "downloads the full image—all blobs, all layers—into a tree artifact"; rules_img "fetches only the manifest and config JSON files at build time"; "Base layers (almost) never move through local machine or remote executors". No benchmark in the article.

Evidence for/against placing byte fetch in an action, from this tool: for: no client download, no tree artifact, cacheable by digest (INFERENCE); against: the author marks the mode experimental, it needs executor egress and registry auth on the executor, and it ships a separate `config` input path for credentials that could leak into the cache.

## 2. rules_oci (bazel-contrib/rules_oci)

Read: `b08db23` (2026-11-13, tag v2.3.5). Files: `oci/private/pull.bzl`, `oci/private/authn.bzl`, `oci/private/util.bzl`, `docs/pull.md`.

- Repo rule downloads the manifest, the config, the platform manifest, and all layer blobs, using `rctx.download` (Bazel's downloader) with `sha256` set when the identifier is a `sha256:` digest (`_download`, `_VERIFIED_DIGEST_PREFIX`). A tag or a non-sha256 digest is fetched with no integrity hash and a warning "result will not be cached". Layers: `# TODO: we should avoid eager-download of the layers ("shallow pull")` (pull.bzl ~line 321). Downloads are async (`block = False`) and waited on. Output: `blobs/sha256/<hex>` in the external repo, exposed through `copy_to_directory(... hardlink = "on")` as a tree artifact `layout` (BUILD template in pull.bzl ~line 216-225), so bytes reach actions as an input tree.
- Cache eligibility: returns `util.repo_metadata(rctx, reproducible = reproducible)` where `reproducible` = identifier is a verified sha256 digest. The comment in `util.bzl` says it is needed "whether local (`--repo_contents_cache`) or remote (`--experimental_remote_repo_contents_cache`)". With a pinned digest the credential file and its env vars are not watched, "so the repo contents cache key" is not tied to the host (`authn.bzl:436-441`).
- Auth in repo rule: reads Docker config / `REGISTRY_AUTH_FILE`, does `WWW-Authenticate` token exchange for a known list of registries, or consumes `--credential_helper`; `docs/pull.md`: "repository rules can not look up HTTP Response headers to perform `WWW-Authenticate` challenges", hence the hard-coded registry map and the `www_authenticate_challenges` attribute.
- RE behaviour (issues read via `gh`): [#572](https://github.com/bazel-contrib/rules_oci/issues/572) (2024-05-11, closed 2024-06-08): a 70 GB image failed under RE at `CopyToDirectory` with "Error while uploading artifact with digest ..."; the reporter had used `no-remote` via `import_tags` in rules_docker; the maintainer pointed to `--modify_execution_info=CopyToDirectory=...` as workaround and then fixed it with PR #573 (adds `bazel_tags`, still an attribute in current `pull.bzl`). [#634](https://github.com/bazel-contrib/rules_oci/issues/634) (2024-06-20, closed same day as duplicate of #497): "most folks who are still using big legacy images with many layers and files still have to disable rules_oci + rules_docker mnemonic to exclude the outputs from remote caching and remote execution" (author sluongng, not a maintainer). [#497](https://github.com/bazel-contrib/rules_oci/issues/497) "FR: Shallow pull for oci_pull" (2024-02-08, still OPEN on 2026-11-23): opened by a collaborator; says pushing giant blobs to the remote action cache "is slow and takes more time than building the target from scratch" and the downloads cost time; says "We won't be offering this feature by default". A maintainer reply says it did not make the 2.0 roadmap. [#769](https://github.com/bazel-contrib/rules_oci/issues/769) "`oci_load` is incompatible with cross architecture remote execution" (open, not read in full).
- Maintainer statements on scale: the quotes above; no published benchmark.

## 3. rules_js (Aspect)

Read: `f5f3e31` (2026-11-16; latest tag listed v4.0.0-alpha.1). Files: `npm/private/npm_import.bzl`, `npm/private/npm_package_store.bzl`, `docs/troubleshooting.md`, `docs/pnpm.md`; PR [#1538](https://github.com/aspect-build/rules_js/pull/1538) via `gh`.

- Download: repo rule `npm_import` uses `rctx.download(url, integrity/sha256, canonical_id = download_url, ...)` through the Bazel downloader ("This uses Bazel's downloader to fetch the packages", file header). The tarball stays in the repo as `_TARBALL_FILENAME`.
- Extraction, default path (no lifecycle hooks, no patches, not `extract_full_archive`): the repo rule extracts only `package/package.json` ("`_download_and_extract_archive(rctx, package_json_only = True)`") and sets `package_src = _TARBALL_FILENAME`. The rule `npm_package_store` then declares a directory output and runs one action per package, mnemonic `NpmPackageExtract`: `bsdtar --extract --no-same-owner --no-same-permissions --strip-components 1`, with the tar binary resolved from a toolchain (`@tar.bzl//tar/toolchain:type`, `ctx.toolchains[tar_lib.toolchain_type]`), `execution_requirements = {"supports-path-mapping": "1"}`. The action is cacheable and remotely executable; the tar is an exec-platform toolchain.
- With lifecycle hooks, patches or `extract_full_archive`: extraction happens in the repo rule (`extract_full_archive` branch), and hook actions run under `lifecycle_hooks_execution_requirements`, default `["no-sandbox"]`; the attribute doc suggests `["no-sandbox", "requires-network"]`, and `docs/pnpm.md:352` shows `["no-sandbox", "no-remote-exec"]` for one package.
- Reasoning: PR #1538 (merged 2024-03-27): "Should mostly fix [#1412]. ... no longer use a CopyDirectory action with a source directory input to copy into the virtual store. Instead they will use a tar toolchain to extract the package .tgz directly into the virtual store." Issue [#1412](https://github.com/aspect-build/rules_js/issues/1412) (2023-12-19): flaky "Copying directory ... failed ... (No such file or directory)" on about 2 percent of builds with `--remote_download_minimal`, Bazel 6.4.0. The `tar` branch is "the majority of packages".
- Costs reported by the project: `docs/troubleshooting.md` § "Remote cache and npm package extraction": one `NpmPackageExtract` action per package; "Remote caches generally store tree artifacts file-by-file, so caching these actions can involve many network round trips, and in a cache-only setup (no remote execution) it may be slower than simply re-extracting the tarballs locally"; suggests `--modify_execution_info=NpmPackageExtract=+no-remote-cache`; "With remote execution the extracted outputs are needed in the remote CAS anyway, so there you most likely want to keep these actions cached ... There is no setting that is right for everyone". Issue [#2715](https://github.com/aspect-build/rules_js/issues/2715) (2026-02-03): warm-remote-cache no-change build 10 s versus 124 s cold, time spent in `NpmPackageExtract`. Issue [#1637](https://github.com/aspect-build/rules_js/issues/1637) (2024-04-10): archives with non-searchable directories (`drw-r--r--`) make cache writes fail, and "the executors [fail] with the same permission denied", "started with the latest feature of extracting tars via bsdtar". Commit `92526e34` (2026-07-09) "perf: reduce per-action overhead registering NpmPackageExtract ...".

## 4. rules_multitool and bazel-lib toolchains

### rules_multitool

Read: `ab911f2` (2026-11-15). Files: `multitool/private/multitool.bzl`, `hub_repo_template/toolchain_info.bzl.template`, `hub_repo_tool_template/tool.bzl.template`.
- Layout (file header): one hub repo `[hub]` holding all `toolchain()` declarations and a `tool` target, plus one repo per tool and platform `[hub].[tool].[os]_[cpu]` holding the binary (`kind = file` via `rctx.download`, `kind = archive` via `rctx.download_and_extract` plus a symlink to the member, `kind = pkg` via `pkgutil`, which sets `reproducible = False`). Each uses `rctx.repo_metadata(reproducible = ...)` when available. The header states "a conscious decision" to keep the Bazel toolchain code in the hub so dependencies run one way.
- Registration: the hub's `native.toolchain(...)` entries have only `target_compatible_with` (cpu, os); `exec_compatible_with` is not set anywhere in `multitool/` (grep). The `toolchain_info` target refers to the per-platform binary repo by label. INFERENCE: under standard Bazel toolchain resolution, only the repo of the resolved toolchain is fetched (bazel-lib's `bats_toolchain.bzl:42` comment says the same: "Bazel only fetches the repository referenced by the toolchain attribute once the toolchain is actually resolved"). Issue [#17](https://github.com/bazel-contrib/rules_multitool/issues/17) (2024-03-08, closed): "Tools are registered for both exec and target platform ... When exec != target we ought to be able to reproduce that a wrong tool is used"; open follow-up [#124](https://github.com/bazel-contrib/rules_multitool/issues/124) (2026-03-02, closed): `@multitool//tools/<tool>` on an arm64 host picks arm64 when building for linux_amd64 images. Both state the tool is selected by one platform axis for both uses. Whether #124's fix changed templates was not traced; the template at `ab911f2` still shows `target_compatible_with` only.
- Tool bytes under RE: the `tool` rule does `ctx.actions.symlink(output, target_file = toolchain.executable)`; the binary is a file in an external repo, so it reaches executors as an action input uploaded to the CAS from the client's external repo (INFERENCE; not run). No remote-exec statement in `readme.md`. [#84](https://github.com/bazel-contrib/rules_multitool/issues/84) "Document usage of Bazel cache when using the rules" is open.

### bazel-lib

Read: `a3fdb1c` (2026-11-10). Files: `lib/private/source_toolchains_repo.bzl`, `copy_directory_toolchain.bzl`, `zstd_toolchain.bzl`, `coreutils_toolchain.bzl`, `bats_toolchain.bzl`.
- Per-platform repos each doing `rctx.download` (or `download_and_extract`) with a pinned sha256, plus a toolchains repo whose `toolchain()` entries use `exec_compatible_with = <os/cpu constraints>` and point to `@<name>_<platform>//:...` (copy_directory: line 139). `source_toolchains_repo.bzl` header quotes the Bazel guidance: "Bazel will not need to analyze all targets referenced by toolchain.toolchain attribute ... The 'complex computation' in our case is simply downloading large artifacts. This guidance tells us how to avoid that." All return `repo_metadata(reproducible = True)` where supported. So a cross-platform RE build fetches, on the client, the repos for the toolchains it resolves; with `exec_compatible_with` that is by exec platform, which differs from rules_multitool's target axis.
- Lazy fetch article: see section 5.

## 5. Aspect article "lazy tool fetching"

URL `https://aspect.build/blog/lazy-tool-fetching-under-bazel` redirects to a page titled "Bazel: Avoiding eager fetches" by Alex Eagle, dated 2022-03-30 (`date:` field in the raw page). Raw HTML grepped for `requires-network`, `no-cache`, "gave up": no hit. So the earlier community lane's search-snippet paraphrase ("an action tagged `requires-network` gave up Bazel's caching; moved to `http_file`") is NOT supported by this article text and should be treated as UNVERIFIED/unsourced.

What the page says (raw page text): the repository cache "does not cache the external repository that Bazel installed on the disk - rather it only caches certain network fetches which had a sha256 sum or integrity hash and were fetched by the Bazel built-in downloader"; tools such as npm and pip "do their own fetches, so those aren't cached by Bazel"; recommended steps are: profile; check in generated output with a freshness test; or "defer the work from a repository rule to an action that runs later in the BUILD graph"; avoid BUILD files that load from external repos and hold unrelated targets; add a CI guard against unwanted fetches. It does not discuss toolchain registration, requires-network, or remote execution. The "lazy toolchain repo" mechanism it is named after is the bazel-lib/Bazel docs pattern in section 4 (separate toolchain repo, platform repo fetched on resolution), quoted from `source_toolchains_repo.bzl`, not from this article.

## 6. rules_python, rules_go, toolchains_llvm

### rules_python

Read: `d9c0896` (2026-11-23; latest tag v0.7.0 per `ls-remote`, which may lag releases). Files: `python/private/pypi/whl_library.bzl`, `whl_file_repo.bzl`, `whl_archive.bzl`, `patch_and_extract_whl.bzl`.
- Wheel download and extraction both happen in repo rules. `whl_library` docs: "URL wheels are downloaded temporarily for extraction, then removed. A sibling `<name>_whl` repository supplies the original archive lazily, when a wheel target is requested. Extraction does not depend on it." So: `whl_file_repo` (`rctx.download`, `repo_metadata(reproducible = True)`) is the lazy source of the `.whl` bytes; `whl_archive` extracts in the repo (`patch_and_extract_whl`, reproducible when extraction is). No extraction action. Patches go into the wheel-file repo. `pip_archive` (the other branch) runs `pip` in a repo rule. Requirements-to-index lookups use `simpleapi_download.bzl`.
- Reasoning: not stated in the files read; UNVERIFIED beyond the doc string.
- `python/private/pypi/pip_compile.bzl:165-167` tags lock-update actions `requires-network`, `no-remote-exec`, `no-sandbox`; `python/uv/private/lock.bzl:632-633,741-743` tags the uv lock action `no-cache` and `requires-network` (doc comment: "This target will have `no-cache` and `requires-network` added to its tags").

### rules_go

Read: `2a638a8` (2026-11-17). `go/private/sdk.bzl`: `_remote_sdk` does `ctx.download_and_extract(url, stripPrefix, sha256, auth)` in a repo rule, one SDK repo per platform; `_sdk_build_file` writes `exec_compatible_with` from `GOOS_CONSTRAINTS`/`GOARCH_CONSTRAINTS`; `_go_toolchains` generates the `toolchain()` repo; `repo_metadata(reproducible = True)` at line 128. `go_download_sdk` and the host-SDK path (`_detect_host_sdk` runs `go env GOROOT` on the client) both exist. Go modules are not fetched in rules_go: `go_repository` lives in Gazelle (`bazel_gazelle`), not read here: UNVERIFIED.

### toolchains_llvm

Read: `bae4d2e` (2026-11-23). `toolchain/internal/llvm_distributions.bzl` `download_llvm` uses `rctx.download_and_extract` in a repo rule; the LLVM distribution is selected by host OS/arch. `toolchain/internal/configure.bzl:441-449` comment: "Since all the toolchains rely on downloading the right LLVM toolchain for the host architecture, we don't need to explicitly specify `exec_compatible_with` ... If the host and execution platform are not the same, then host auto-detection based LLVM download does not work and the user has to explicitly specify the distribution of LLVM they want." README 4.: "All the above options rely on host OS information, and are not suited for docker based sandboxed builds or remote execution builds. Such builds will need a single distribution version specified through the `distribution` attribute, or URLs ... with an empty key". `extra_exec_compatible_with` exists as an attribute. (`toolchains_llvm_bootstrapped` from the community lane was not read.)

## 7. rules_nixpkgs (tweag/rules_nixpkgs)

Read: `26cde42` (2026-11-17), `docs/remote-execution-tutorial.md`. "During a Bazel build with rules_nixpkgs, a series of Nix store paths will be created on the local machine ... The challenge with remote execution is to ensure that the executors have those Nix store paths available." Mechanism: with `BAZEL_NIX_REMOTE=<ssh host>` the rules "copy those paths, through SSH, to a remote Nix server. Then the paths can be made available to remote executors through a read-only NFS mount" of `/nix/store`. Bytes are placed out of band, outside the CAS; the Bazel-side graph references store paths. Needs an NFS-visible Nix server and executors that mount it. The tutorial says it cannot give exact RE setup instructions for the backend; the Buildbarn config shown is two flags. Tweag blog (2024-02-29) and issue #180 were read in an earlier lane, not re-read.

## 8. Kleaf (Android kernel build)

Read raw: `kleaf/bazelrc/network.bazelrc`, `rbe.bazelrc`, `hermetic_cc.bazelrc`, `kleaf/docs/network.md` (android.googlesource.com, `main`, 2026-11-23; commit hash not captured).
- Correction to the community lane: `network.bazelrc` blocks external DOWNLOADS BY REPOSITORY RULES, not action network. It sets `common:no_internet --repository_cache=`, `--distdir=` and `--repository_disable_download` ("downloading using ctx.download{,_and_extract} is not allowed during repository fetching") and `common --config=no_internet` as the default; `--config=internet` relaxes it. `docs/network.md`: "In a near future, `--config=no_internet` will be set by default, disabling external downloads, except when using `--use_prebuilt_gki`." No `requires-network` or `sandbox_default_allow_network` appears in any file in the `kleaf/bazelrc` directory (grepped each file).
- `rbe.bazelrc`: `--config=remote` sets a custom RBE platform as host, exec and target platform, `--remote_executor=grpcs://remotebuildexecution.googleapis.com`, `--incompatible_strict_action_env`.
- How external tools are provided: not established. `hermetic_cc.bazelrc` selects a hermetic C toolchain and sets `BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1`. Where the toolchain bytes come from (prebuilts checked out by the repo manifest) is UNVERIFIED here.

## 9. Rulesets that download in a build action

| Where | Action | Tags / requirements | Statement |
|---|---|---|---|
| rules_img `DownloadBlob` (section 1) | registry blob download by digest | rule target `requires-network`; action `supports-path-mapping` | marked EXPERIMENTAL; no cache tag so the result is remotely cacheable |
| rules_img `PushImage` | registry upload | `requires-network`, `supports-path-mapping` (overridable) | "non-hermetic"; designed for RE pools |
| rules_python `pip_compile`, `uv lock` | lock update | `requires-network`, `no-remote-exec`, `no-sandbox` (pip_compile); `no-cache`, `requires-network` (uv lock) | lock regeneration, not delivery |
| rules_js lifecycle hooks | npm postinstall | default `no-sandbox`; `requires-network`, `no-remote-exec` are user-chosen | `docs/pnpm.md:296-352` |
| rules_foreign_cc `framework.bzl:621-623` | build of a C library | `block-network` unless the user's tags include `requires-network` | default-deny |
| rules_rust `crate_universe/BUILD.bazel:153` | a test | `requires-network` | |
| Aspect bazelrc preset (`rules_js/tools/preset.bazelrc:197-202`) | all actions | `--nosandbox_default_allow_network`; "Developers should tag targets with `tags=["requires-network"]` to be explicit" | stated reason: "Avoids accidental non-hermeticity in actions/tests which depend on remote services" |
| bazel-lib `lib/private/utils.bzl:24-30` | | lists `no-remote-exec`, `no-remote`, `local`, `requires-network`, `block-network`, ... as passthrough execution tags | |

Remote cacheability: Bazel does not alter caching for `requires-network` (section 0.1). Of the rows above, only `no-cache`, `no-remote*`, `local` make the action non-cacheable; rules_img's `DownloadBlob` carries none.

Maintainer or author statements found elsewhere (community lane, not re-read): jmmv 2025-09 argues disabling worker network is "the only thing you can and must do"; Tweag 2026-04-02 says Bazel "by design lacks support and features for downloads to take place within build actions". Those two were not re-read in this lane.

## Negative

- The earlier claim, attributed to Aspect, that a `requires-network` action "gave up Bazel's caching" is not in the article text (raw page grepped).
- The earlier Kleaf claim (network off for actions by default) does not match the rc file: it is repo-rule downloads.
- No ruleset read sets `remotable = True` on a repo rule; no use of `--experimental_repo_remote_exec` found.
- No Remote Asset API usage inside any ruleset source read; only CI configs (rules_img's `.bazelrc.buildbuddy`) point `--experimental_remote_downloader` at a backend.
- No source was found where a ruleset states that executor egress is policy-blocked in a real deployment; rules_img only documents that some setups block it.
- rules_multitool `readme.md` has no RE or cache statement.
- `bazel fetch` / `bazel vendor` coverage of rules_img lazy layers is INFERENCE only.
- GitHub MCP server unavailable (auth error); `gh` CLI was used instead. Slack threads cited in rules_multitool #17 were not reachable.
- Not read: rules_rs/crate_universe fetch path, rules_oci `oci_pull` tests, Gazelle's `go_repository`, toolchains_llvm_bootstrapped, rules_pkg, BuildBuddy/EngFlow/Buildbarn/NativeLink backend behaviour for `requires-network` or Remote Asset.

## Leads

- Backend semantics for `requires-network` and Remote Asset (BuildBuddy, EngFlow, NativeLink, Buildbarn): this lane found the tag is invisible to Bazel's remote path, so executor policy is the open question.
- rules_img maintainers (Tweag): why `lazy` is still marked experimental and what failures they saw under RE; read open issues in bazel-contrib/rules_img.
- Run an experiment against the Bazel 9.3.3 remote repo contents cache with a repo that has same-repo symlinks only versus cross-repo symlinks, to confirm the silent exclusion (#31456 first checkbox).
- `bazel fetch` / `bazel vendor` behaviour with `--experimental_remote_repo_contents_cache` (#31456 "decide whether fetch should materialize repos"; #31005).
- rules_multitool #124 and its fix: which platform axis a binary from a hub gets in cross builds.
- Gazelle `go_repository` and rules_rust crate_universe: module and crate fetch placement (repo rule versus action).
- Kleaf prebuilts provisioning (the `repo` manifest path versus Bazel fetch).
