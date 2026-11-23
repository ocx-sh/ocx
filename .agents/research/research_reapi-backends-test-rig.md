# Research: open-source REAPI backends for a conformance test rig

## Metadata

**Date:** 2026-11-18
**Lane:** competitive scan (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** project READMEs and docs as cited; release years and most symlink behaviour UNVERIFIED
**Expires:** 2027-05-18
**Caveat:** findings only, no recommendation.

Today is 2026-11-18. The GitHub releases pages omit the year, so all release dates are UNVERIFIED as to year. Rows quote the README or docs I could read, and anything else is UNVERIFIED.

### Comparison table

| | Buildbarn | BuildBuddy OSS | Buildfarm | NativeLink | bazel-remote / `--disk_cache` |
|---|---|---|---|---|---|
| 1. Signals | 139 stars on bb-deployments. Component images tagged 2026-11-02, so active. No GitHub releases; images are date-tagged. | 807 stars. v2.325.0 on 13 Nov, with a release every few days. Backed by BuildBuddy Inc. | 780 stars, Apache-2.0. v2.18.0 on 27 Oct, roughly 2-4 releases a year. 53 open PRs. | About 1.6k stars. v1.8.6 on 17 Nov, with 6 releases in about 4 weeks. License is FSL-1.1-Apache-2.0 (Apache after 2 years). Company-backed (TraceMachina). | 792 stars. v2.6.4 (20 Oct per the page, year and ordering unclear). Cache only. |
| 2. Smallest local setup | Official docker-compose (`./run.sh`). It starts frontend, 2 storage, scheduler, portal plus Postgres, and 2 worker/runner pairs (FUSE and hardlinking). No single-binary mode. No footprint stated. | Docs I fetched cover enterprise/Kubernetes only. "RBE only configurable in the Enterprise version" (config-rbe page). A local OSS executor recipe was not found. | Redis, a server and a shard worker run via `bazel run`, or `./examples/bf-run start`. Linux only, tested. No footprint stated. | `deployment-examples/docker-compose` has a multi-worker compose, with the cache on :50051 and the executor on :50052. The README is inconsistent on ports. The Docker image is x86_64 only. | `bazel-remote` is a single binary, no execution. `--disk_cache` is in-client, no execution. |
| 3. Executor rootfs | `container-image` is an *image tag* the operator pairs to a runner container. The runner container is a generic image plus `bb_runner` copied in by `bb-runner-installer`. `bb_runner_bare` has no userland. Compose waits in a shell loop, but the entrypoint could be replaced (inference). The input root is built by native hardlinking or by FUSE/NFSv4. | `container-image` property (`docker://…`). Isolation is `workload-isolation-type`: `oci` (default), `firecracker`, and self-hosted `docker`/`podman`/`sandbox`/`none`. A no-shell image is not addressed in the docs. Image `CMD` is ignored, `ENTRYPOINT` is honoured. | Not read; the wrappers `as-nobody.c`, `cgexec-wrapper` and `macos-wrapper.sh` exist. UNVERIFIED. | Per the gist diff, the compose reclient worker maps the `container-image` property into a `CONTAINER` env var. Rootfs behaviour is otherwise UNVERIFIED. | n/a |
| 4. Network | The compose runners use `network_mode: none`, so network is off by default. It is a container-level choice, not a platform property. | `network` property: `off` (default for oci/podman/docker/sandbox), `external` (default for firecracker), `host`. The older `dockerNetwork` is deprecated. | UNVERIFIED | UNVERIFIED | n/a |
| 5. Symlinks | Symlinks and `SymlinkAbsolutePathStrategy`: UNVERIFIED. Related: bb-remote-execution PR #213, a draft opened 2026-02, fixes absolute-symlink checks on Windows paths. | UNVERIFIED | Advertises absolute targets as allowed only when `allowSymlinkTargetAbsolute` is on (README). Default is off. | UNVERIFIED | n/a |
| 6. Remote Asset | Not in bb-storage. A separate repo, `bb-remote-asset`, has 13 stars, no releases, and a `fetcher` config. FetchBlob is UNVERIFIED. | The rules_python BuildBuddy config uses it as `--experimental_remote_downloader`, so the cloud service implements it. OSS status UNVERIFIED. | `FetchBlob` is supported, with checksum and header qualifiers. `FetchDirectory` returns UNIMPLEMENTED. No Push. | UNVERIFIED | Experimental subset of Fetch behind `--experimental_remote_asset_api`. |
| 7. Multi-platform | One scheduler can serve many workers, each declaring a `platform`. The runner config supports `case_insensitive` for WinFSP and macOS NFSv4 notes. Windows and macOS worker support is plausible, not stated. | `OSFamily` = linux/darwin/windows and `Arch` = amd64/arm64. darwin and windows are self-hosted executors only. | Linux tested. The macOS wrapper hints at mac. Rest UNVERIFIED. | A single node runs multiple workers via `platform_properties` (gist). Native Windows is unsupported for the Nix build; WSL2 works. macOS builds via Nix. Worker support per OS is UNVERIFIED. | n/a |
| 8. Strict or lenient | `bb_worker` has `input_root_character_device_nodes` and per-instance-name worker pools. FUSE mode makes inputs read-only and lazy; the native mode hardlinks. | Default image is a BuildBuddy Ubuntu image, not a minimal one. | Absolute symlink handling is gated by config. | UNVERIFIED | `--disk_cache` fakes nothing about exec. |

### Key findings
- The REAPI enum is `SymlinkAbsolutePathStrategy`. `DISALLOWED` returns `INVALID_ARGUMENT` on input absolute symlinks and `FAILED_PRECONDITION` on output ones; `ALLOWED` lets targets escape the input root ([remote_execution.proto](https://raw.githubusercontent.com/bazelbuild/remote-apis/main/build/bazel/remote/execution/v2/remote_execution.proto)). This is the clearest axis where backends differ.
- Buildbarn splits `bb_worker` and `bb_runner` so that inputs are hardlinked and actions cannot mutate them ([bb-remote-execution](https://github.com/buildbarn/bb-remote-execution)). `bb_runner_bare` is a static runner with no userland, and `bb_runner_installer` mounts it into any image. For a no-shell test image this is the best-documented option in this research. Whether an image with no shell and no coreutils actually runs is not confirmed.
- The Buildbarn compose config ([bb-deployments](https://github.com/buildbarn/bb-deployments), [docker-compose.yml](https://raw.githubusercontent.com/buildbarn/bb-deployments/master/docker-compose/docker-compose.yml)) has a privileged FUSE worker and an unprivileged hardlinking worker. Using the hardlinking worker avoids the privilege requirement.
- Bazel's own reference worker `//src/tools/remote:worker` implements both cache and execution via gRPC. Sandboxing is Linux only and its README flags a kernel slowdown for parallel sandboxed actions. Its README states no maintenance status, and I did not check its commit history ([README](https://raw.githubusercontent.com/bazelbuild/bazel/master/src/tools/remote/README.md)). It is the lightest local server, but I did not verify its symlink handling.
- Ruleset CI practice: rules_python runs RBE on BuildBuddy cloud for Linux and blanks `--remote_executor` on macOS and Windows ([.bazelrc](https://raw.githubusercontent.com/bazelbuild/rules_python/main/.bazelrc), [buildbuddy.bazelrc](https://raw.githubusercontent.com/bazelbuild/rules_python/main/buildbuddy.bazelrc)). rules_go and rules_oci `.bazelrc` files show no RBE; rules_oci's CI-only `ci.bazelrc` was not read. Rulesets use a hosted service, not a self-run backend.
- Bazel's community list names Buildbarn, Buildfarm, BuildGrid and NativeLink as self-service ([page](https://bazel.build/community/remote-execution-services)).
- BuildGrid is another open-source REAPI server, Apache-2.0, on GitLab ([repo](https://gitlab.com/BuildGrid/buildgrid)). Recency was not visible on the page.
- A symlink gotcha unrelated to any one backend: symlinks produced by `ctx.actions.symlink` / `declare_symlink` come back as regular files under remote execution, in Buildbarn and other backends alike. Bazel documents this limit ([bazel#16361](https://github.com/bazelbuild/bazel/issues/16361), [repro](https://github.com/castler/buildbarn_bazel_symlink_issue_repro)). It is not a server bug.

### negative:
- No server's `GetCapabilities` symlink strategy was confirmed for Buildbarn, BuildBuddy, NativeLink or bazel-remote.
- No primary source confirmed that a scratch or distroless-static image runs for any backend.
- No local-run recipe for the BuildBuddy open-source executor was found; the pages I read cover enterprise setup only.
- NativeLink's worker rootfs choice, network switch, symlink behaviour and Remote Asset support were not confirmed.
- Buildfarm's network switch and sandbox options were not confirmed.
- Resource footprints are not stated for any backend.
- Bazel 9.2 client compatibility is not stated for any backend.
- Whether any GitHub-hosted `ubuntu-latest` runner runs any of these was not checked.
- No source confirmed that Bazel's reference worker is still maintained.

### leads:
- Read `bb-remote-execution` runner and worker Go sources, plus the bb-storage capabilities code, for the symlink strategy and input-root checks. Source files were not searchable through the fetch tool.
- Read NativeLink `worker.json5`, the Remote Asset API implementation and the docs.nativelink.com platform-properties page (redirected from nativelink.com/docs).
- Read BuildBuddy's `docs/` folder for the self-hosted executor config, and `.github` in rules_python and rules_oci for `ci.bazelrc`.
- Check the bazelbuild/bazel commit history of `src/tools/remote` for maintenance status.

Sources: [bb-deployments](https://github.com/buildbarn/bb-deployments), [bb-remote-execution](https://github.com/buildbarn/bb-remote-execution), [bb_worker.proto](https://raw.githubusercontent.com/buildbarn/bb-remote-execution/master/pkg/proto/configuration/bb_worker/bb_worker.proto), [BuildBuddy RBE platforms](https://www.buildbuddy.io/docs/rbe-platforms/), [BuildBuddy config-rbe](https://www.buildbuddy.io/docs/config-rbe/), [Buildfarm](https://github.com/buildfarm/buildfarm), [Buildfarm quick start](https://buildfarm.github.io/buildfarm/docs/quick_start/), [NativeLink](https://github.com/TraceMachina/nativelink), [NativeLink compose](https://github.com/TraceMachina/nativelink/tree/main/deployment-examples/docker-compose), [bazel-remote](https://github.com/buchgr/bazel-remote), [NativeLink compose gist](https://gist.github.com/adam-singer/a7aa64abf6445eb5752f418a457ca25a).
