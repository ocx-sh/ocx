# Research: Signing dogfood, dev channel and release pipeline

## Metadata

- Date: 2026-11-13
- Lane: codebase recon (read-only; no builds, pushes, dispatches)
- Commits read (`origin/main` unless noted): ocx `f5e6e66a4`; ocx-mirror `050aab5`; setup-ocx `25fa771`; ocx-indexbot `a79f7f3` (local HEAD `c743455`, not used); ocx-catalog `159f314` (local HEAD `d560f56`, not used); index `6c67672b` (local HEAD `f8c3b28f`, not used); e2e-mirror `a4c9fc6`; server-hetzner1 `9ae3dac`; additionally www-setup `094649b` (installer source, found while answering Q7); worktrees read at HEAD: e2e-package `11f569c`, e2e-index `e8437d5` (both are standalone clones of ocx-sh/e2e-package and ocx-sh/e2e-index, branch `main`, not git worktrees of ocx).
- Convention: "(inferred)" marks anything not read directly from a file. Paths are relative to the named repo.

## 1. `dev.ocx.sh`

**What it is.** Two things behind one host name, both in `server-hetzner1`:
- `/v2/...` (the OCI registry API) is proxied to a JFrog Artifactory docker repo `sh-ocx-oci-dev`: `nginx/config/main/conf.d/ocx-sh-10-dev.conf:14-16` (`proxy_pass http://jfrog_router/artifactory/api/docker/sh-ocx-oci-dev/v2/$1`). `ocx-sh-10-grim.conf:14-16` is the same pattern for `sh-ocx-oci-grim`.
- `/` is proxied to Cloudflare Pages `dev.ocx-website.pages.dev` (the dev copy of the website): `ocx-sh-10-dev.conf:20-24`.
- It is an OCI registry only; no index is served from it (the index tree is `index.ocx.sh`, outside this lane). Pulls are anonymous: `ocx-mirror:.claude/skills/e2e-test/references/dev-channel.md:19-21,79`.
- `server-hetzner1/registry/docker-compose.yml` (a `registry:3` + `joxit/docker-registry-ui` pair, `REGISTRY_STORAGE_DELETE_ENABLED=true`) exists but no nginx conf routes to it (grep of `nginx/config` finds only artifactory upstreams). Whether it is live or legacy: unknown.
- `server-hetzner1` docs say nothing about the dev repo's retention, cleanup policy or credentials (`artifactory/README.md` is only an EULA/chown snippet; the only retention text is Tempo/Grafana in `monitoring/README.md:125-130`).

**Workflows that publish there.**
- ocx: `.github/workflows/deploy-dev.yml` ("Deploy Dev"). Trigger: `workflow_dispatch` only (`:15`), serialized `concurrency: deploy-dev-${{ github.ref }}`, `cancel-in-progress: false` (`:22-24`). It runs on any ref (no branch guard seen), so a branch build can be published by dispatching on that ref.
  - Flow: `compute-version` (git-cliff `--bumped-version`, `:50-70`) -> `build-linux` (musl x2, cargo-zigbuild), `build-darwin` x2, `build-windows` (cargo-xwin container, msvc x2) -> `scan` (`scan-binaries.yml`, `min-files: 6`, `:180`) -> `publish` (`oci-publish.yml`, `:184-197`).
  - Build env bakes `__OCX_BUILD_VERSION=<core>-dev+<TS>` and `__OCX_BUILD_CHANNEL=dev` (`:83,110,134`).
  - Publish target: `dev.ocx.sh/ocx/cli:<core>-dev_<TS>` (default variant => `<repo>/cli`, `oci-publish.yml` repo_path logic). Six targets: linux musl amd64/arm64, darwin amd64/arm64, windows msvc amd64/arm64 (`deploy-dev.yml:192`). Environment `dev.ocx.sh`.
- ocx-mirror: `.github/workflows/deploy-dev.yml` (`workflow_dispatch` `:8`), tag `<core>-dev_<TS>` (`:51`), `build-matrix.yml` then its own `oci-publish.yml`, target `dev.ocx.sh/ocx/mirror` (`:71`, `oci-publish.yml:134` METADATA `packaging/metadata.json`). Its platform map uses `windows-gnullvm` triples (`oci-publish.yml:130-131`), unlike ocx's msvc.
- Both workflows validate the registry against an allowlist `dev.ocx.sh|ghcr.io` (ocx `oci-publish.yml:199`; mirror `:107`).

**Tag naming.**
- Unique tag: `X.Y.Z-dev_YYYYMMDDHHMMSS` (UTC stamp frozen once per run, comment at `deploy-dev.yml:57-60`). `X.Y.Z` is the next version git-cliff would bump to, not the current one (e.g. e2e-mirror pins `0.7.0-dev_20260903181659` while releases were 0.6.x).
- Rolling/cascade tags: `ocx package push --cascade` (`oci-publish.yml:360`). A pre-release build cascades only into its own pre-release (`website/src/content/docs/publishing/snapshot-tracks.md:24`), so `0.5.0-dev` floats (observed in `.claude/artifacts/e2e_evidence_e1_e2a.md:11`: `dev.ocx.sh/ocx/cli:0.5.0-dev` -> build `0.5.0-dev_20260726083641`; `handover_announce_fleet_rollout.md:118`). `latest`/`X.Y`/`X` do not move.
- Also pushed per run for the default variant: the repository description (`ocx package description push ... --readme packaging/ocx/CATALOG.md --logo assets/logo.svg`, `oci-publish.yml:384`).
- Not pushed with `--no-keep-tag` (flag absent in `oci-publish.yml:355-365`), so keep tags exist for every dev platform manifest (see Q2: this defeats `ocx package prune`).

**Announced to an index?** No. `oci-publish.yml` has `announce` input default `false` (`:~50`, ocx) and `deploy-dev.yml` does not pass it; the script prints "skipping index announce ... this publish does not announce" (ocx `oci-publish.yml:240-250` region). Stated rationale: a dev tag does not exist on the registry the index root points at (ocx `oci-publish.yml` announce description; ocx-mirror `oci-publish.yml:27-31`). Only the release publish (`post-release-oci-publish.yml:52`, ghcr.io, `announce: true`) announces.

**Removal of old builds.** No mechanism found in either repo's workflows or taskfiles (grep for `package prune`, retention, cleanup in `.github`, `taskfile.yml`, `taskfiles` finds only the bazel-cache-rc action). `ocx package prune` would refuse these tags: not in the index => exit 75 (`snapshot-tracks.md:119-132`), and keep tags would keep storage reachable (`:26-32`). Any Artifactory-side cleanup policy would live in the Artifactory UI, not in these repos (inferred; unread). Existing builds are therefore only ever added (inferred from absence).

**Credential names only.** Repository secrets `REGISTRY_USER`, `REGISTRY_TOKEN` passed to the reusable workflow (ocx `deploy-dev.yml:195-196`); GitHub Environment `dev.ocx.sh` gates them. In the script they become `OCX_AUTH_dev_ocx_sh_USER` / `OCX_AUTH_dev_ocx_sh_TOKEN` (ocx `oci-publish.yml:213-215`). Optional `OCX_ANNOUNCE_TOKEN` is never provided on the dev path. ocx-mirror's build legs additionally read `BAZEL_CACHE_READ_AUTH`. No repository variables referenced.

## 2. The `ephemeral` feature

**Definition.** A per-tag-row boolean marker in the index root: `"ephemeral": true` on a tag row means "a snapshot meant to disappear". Only a JSON `true` counts; `false`, `null`, `"true"`, `1`, objects, arrays all parse as durable (`ocx:crates/ocx_index/src/wire.rs:91-103`, tests `:425-457`). It is independent of `yanked` (`wire.rs:452`).

**CLI surface in ocx.**
- `ocx package announce --ephemeral` (`crates/ocx_cli/src/command/package_announce.rs:140-144`): marks each row the run *adds*; never changes an existing row's marker; conflicts with `--refresh` (tests `:722-765`); composes with `--tags`, `--tags-file`, `--tags-from-registry`. Docs: `website/src/content/docs/publishing/announce.md:125-147`.
- `ocx package push --no-keep-tag` is the paired flag; the help text says "push ephemeral builds with `--no-keep-tag`, or prune frees no storage" (`package_push.rs:27`).
- `ocx package prune` (`package_prune.rs:31-37,93`): deletes registry tags only if the served index lists them ephemeral. Listed durable => exit 81; in registry but not in index => exit 75; `--force` overrides 81. Full table: `snapshot-tracks.md:119-132`. Prune never writes the index.
- `ocx package announce --refresh` / `--tags-from-registry`: remove an ephemeral row whose tag is gone from the registry; a durable row is kept and reported in `durable_missing` (`announce.md:139-142`; `package_announce.rs:122-131`).
- Whole recipe (push, prune, `cascade repair`, announce `--ephemeral`): `snapshot-tracks.md:15-70`.

**Resolution (consumer side).** ocx-index treats an ephemeral local row specially: a bare `ocx index update` drops an ephemeral local row the served root no longer lists; a durable pin the root lacks survives (`website/.../explanation/local-index.md:146-148`; `.claude/rules/subsystem-oci.md:314`). `ocx --remote` resolving a tag the root lacks drops the local ephemeral row, then exits 79 (`crates/ocx_index/src/chained_index.rs:523-544`, test `:3966`). `ocx index` merge help repeats it (`command/index.rs:46,71`).

**Announce.** `ocx_announce` carries `ephemeral` through `announce.rs`/`announce/pipeline.rs` (35 and 48 mentions); `package announce` against an ephemeral row whose tag is gone removes it in every mode; a durable row is removed only if named via `--tags`/`--tags-file` (`announce.md:139-142`).

**Index bot (ocx-indexbot).**
- Governance lane: a removed row is machine-lane (auto-mergeable) only if it was `ephemeral: true` at the base AND a manifest GET on the canonical registry answers `MANIFEST_UNKNOWN`; otherwise human review. Durable removal always human. `ephemeral` added/removed on an existing row => human review. New row (marked or not) unchanged. (`docs/reference/cli.md:170-186`; code `src/ocx_indexbot/cli/classify_pr.py:99-108`, `core/diff.py:27-44,110,142`; CHANGELOG line: "Accept the ephemeral tag marker; auto-merge a removal only once the registry confirms the tag is gone").
- `reconcile`: an ephemeral row is exempt from the "digest not in scope / registry-existence" escalation, like yanked (`cli/reconcile.py:46,157,250,332-341,372`; `docs/reference/contracts.md:416,888,899`).
- `regenerate` (the bot's own writer) has no `--ephemeral`; it preserves an existing marker and writes new tags durable (`core/regenerate.py:63-66`).
- `stale`: unrelated to the marker; it warns then closes abandoned pull requests (`docs/reference/cli.md:487-497`).
- `ocx-sh/index` contributor doc states the same lane (`index:CONTRIBUTING.md:39-40`). Live data: `index:p/extra-code/container.json` carries `"ephemeral": true` rows (lines 36-61), the only rows seen with the marker in the index checkout.

**Catalog (ocx-catalog).** The detail page draws an ephemeral tag with a bar and the support matrix notes "ephemeral: may be pruned"; `catalog.json` `latest` skips ephemeral tags, so a package whose newest build was ephemeral reports an older `latest` (`docs/src/content/docs/explanation/search-and-versions.md:48-52`, `how-to/upgrade-to-0-6.md:182-189`, `reference/output-layout.md:123`; fixture `scripts/dev-indexes.mjs:24`).

**Not found.** No `ephemeral` use in setup-ocx, e2e-mirror, or in ocx-mirror source (only unrelated "ephemeral port" comments). Neither `deploy-dev.yml` uses the feature (see Q1).

## 3. How a consumer selects an ocx build

**setup-ocx action** (`action.yml:7-47`). Inputs: `version` (default `latest`), `github-token`, `libc`, `project` (default `ocx.toml`; `""` disables), `working-directory`, `groups`, `cache`, `cache-suffix`, `ocx-home`, `managed-config`. No `registry` input and no `index` input. It downloads from GitHub Releases of `ocx-sh/ocx` only: `src/constants.ts:1-2,135-145` builds `https://github.com/ocx-sh/ocx/releases/download/v<version>/ocx-<target>.tar.gz` (fallback `.tar.xz`; Windows `.zip`) and verifies against `<archive>.sha256` (`src/download.ts:127-138`); `latest` resolves via `GET /repos/ocx-sh/ocx/releases/latest` (`src/version.ts:54`). A `version:` therefore must be a published GitHub release tag; a `dev.ocx.sh` build cannot be chosen through this input. After that bootstrap, the `project` step runs `ocx pull` and activates `ocx.toml`, so tools declared there (including a different `ocx`) come from whatever registry the pins name.

**Per-repo pins** (`ocx.toml` / `ocx.lock`):
| Repo | ocx pin | Notes |
|---|---|---|
| ocx | no ocx tool in `[tools]`; `[group.test.tools] ocx-previous = "ocx.sh/ocx/cli:0.6.5_20261007132152"` (`ocx.toml`) | build-qualified tag; `test/src/previous_release.py:1-12,52` resolves it with `ocx env -g test --project ocx.toml`, override `OCX_TEST_PREVIOUS_OCX`, opt-out `OCX_TEST_NO_PREVIOUS_OCX=1`; lock `generated_by ocx 0.6.5` |
| ocx-mirror | `ocx = "ocx.sh/ocx/cli:0.6.3"` (`ocx.toml:9`) | release pin; `.github/workflows/oci-publish.yml:79-86` also `setup-ocx` `version: "0.6.3"`; lock `generated_by ocx 0.6.5` |
| ocx-indexbot, ocx-catalog | no ocx tool (only task, uv, git-cliff, actionlint, lychee, gitleaks) | CI uses `ocx-sh/setup-ocx@25fa771... # v1` for the toolchain; locks `generated_by ocx 0.5.8` |
| index | no `ocx.toml` at all (`grimoire.toml` instead); CI refs `ocx-sh/website/.github/actions/deploy`, no setup-ocx found in `.github` | |
| e2e-mirror | `cli = "dev.ocx.sh/ocx/cli:0.7.0-dev_20260903181659"`, `mirror = "dev.ocx.sh/ocx/mirror:0.7.0-dev_20260903094738"`, `uv` from ocx.sh (`ocx.toml:3-5`) | lock holds six per-platform digests per dev tool (`ocx.lock`, `generated_by ocx 0.6.0`) |
| e2e-package, e2e-index worktrees | no `ocx.toml`/`ocx.lock` in either | both workflows install a released ocx with `setup-ocx ... version: '0.6.6', project: '', cache: false` (e2e-package `release.yml:30-34`; e2e-index `client-e2e.yml:49-53`) |

**How e2e-mirror pins a dev ocx-mirror** (`README.md` "Pinning a dev build" and "Which `ocx` actually runs"): run `gh workflow run "Deploy Dev" --repo ocx-sh/ocx-mirror --ref <branch>`, put the printed tag in `ocx.toml`, run `ocx lock`, commit both. Generated workflows call `ocx-sh/setup-ocx@... version: "0.6.0"` only as the *bootstrap*; setup-ocx then activates `ocx.toml`, so the `ocx` and `ocx-mirror` the push step uses are the dev pins. The push step calls `ocx-mirror` directly, not via `ocx exec`, to keep that (`README.md`; `ocx-mirror:src/command/package/pipeline/generate/templates/workflow.yml:217` comment on `OCX_BINARY_PIN`). The same runbook is `ocx-mirror:.claude/skills/e2e-test/references/dev-channel.md:27-80` (note: it says the generated workflow's `setup-ocx version` is baked from `OCX_CONTAINER_CLI_TAG` in `generate/ci/matrix.rs`, a separate pin).

**How e2e-index `client-e2e` gets a branch-built client today** (`.github/workflows/client-e2e.yml`): `workflow_dispatch` with inputs `ocx_repository` (allowlist `ocx-sh/ocx` or `ocx-sh/e2e-index`, `:36-39`) and `ocx_ref` (40-hex SHA, `:40`); it checks out that ref with `submodules: recursive` into `ocx-src` (`:41-48`), installs released ocx 0.6.6 via setup-ocx only to run `uv` (`:49-53,64`), then compiles the client from source: `cargo build --locked -p ocx` (`:54-58`), and runs the suite with `OCX_E2E_BINARY=ocx-src/target/debug/ocx` (`:60-64`, marker `e2e_public`). No registry round trip; no secrets (`:6`); guarded to `github.repository_id == '1455949056'` (`:24`). The workflow must live on the default branch of e2e-index so it can be dispatched for refs not on ocx main (`:5`).

## 4. ocx-mirror and the ocx sources

- **Submodule**: `external/ocx` -> `https://github.com/ocx-sh/ocx.git` (`.gitmodules`), gitlink pinned at `a536e92ff` (= ocx release commit "release: v0.6.6", `git ls-tree origin/main external/`).
- **Crates linked as path dependencies** (`Cargo.toml:58-71`): `ocx_config`, `ocx_console`, `ocx_env`, `ocx_exit`, `ocx_index`, `ocx_oci`, `ocx_package`, `ocx_sign`, `ocx_util`, `ocx_python`. The workspace `exclude = ["external/ocx"]` (`:21`). Comments (`:48-57`) record that `ocx_shell`/`ocx_announce` are reached via `ocx package push --ci-annotations` and a restated forge vocabulary, and that `ocx_cli` is not linked (its `tracing_init` is a verbatim copy in `src/tracing_init.rs`).
- **Also shells out to an `ocx` binary.** `crates/ocx_mirror_pipeline/src/ocx_cli.rs:29-38`: `OCX_BINARY_PIN` if set (set by `ocx exec`), else `ocx` on `PATH`; `push.rs:426` uses it, plus `ocx_cli/{create,push,sign}.rs`. `OCX_BINARY_PIN` is on the forwarded-env whitelist (`ocx_cli.rs:49`).
- **Where that binary comes from**:
  - Production/CI publish: `setup-ocx` release 0.6.3 (`oci-publish.yml:78-86`: "this repo never builds ocx").
  - Local/CI toolchain: `ocx.toml:9` `ocx.sh/ocx/cli:0.6.3`; under Bazel `//test:acceptance` the runner receives `@tools//:ocx` (`test/BUILD.bazel:35`, `test/bazel_accept.sh:19,66`: `OCX_COMMAND="$ocx" OCX_TEST_BINARY="$ocx"`, i.e. the pinned release, not the submodule build). CI acceptance (`.github/workflows/acceptance.yml:165`) puts `ocx` and `uv` on PATH from the project toolchain.
  - Test fixtures: `test/conftest.py:212-224` `ocx_binary` = `OCX_COMMAND` else `test/bin/ocx`; `real_ocx_binary` (`:241-266`) = `OCX_TEST_BINARY` else `cargo build --release --bin ocx` inside `external/ocx` (used by blob-mount tests). Unit tests stub it via `OCX_BINARY_PIN` (e.g. `lock_derive.rs:546`).
  - Historical oracle runs used `OCX_COMMAND`=`OCX_TEST_BINARY`= the released ocx 0.6.2 (`.agents/goal/bazel-crate-split.md:59`).

## 5. Local publishing

**Tasks that package or push ocx.** None for `ocx/cli`. The only task that packages a locally built binary as an OCX package is `release:sdkgen:package` / `release:sdkgen:dry-run` for `ocx-sdkgen` (`taskfiles/release.taskfile.yml:~80-125`: `ocx package create ... -m packaging/sdkgen/metadata.json -o OUT/ocx-sdkgen.tar.xz -p <platform> --force`); it explicitly "pushes nothing". Nothing in `taskfile.yml` or `taskfiles/` runs `ocx package push`. The `ocx/cli` packaging recipe exists only inside CI (`oci-publish.yml:~320-365`: install binary at `bin/ocx`, add LICENSE and LICENSE-THIRD-PARTY.md, `ocx package create <dir> -m packaging/ocx/metadata.json -o ...tar.xz -p <platform>`, then `ocx package push --identifier <reg>/<repo>/cli:<tag> --platform <p> --cascade ...`). `release:provenance:proof` (`release.taskfile.yml:62-77`) shows `scripts/release_provenance_check.py --scan` goes red on a `__testing` build.

**Local registries the tests start.**
- zot: per pytest session, three instances (primary, promotion target, promotion prod) on kernel-chosen free ports (`test/src/zot.py`, `test/docker-compose.yml:13-14`, `CONTRIBUTING.md:46-50`); `zot` comes from the project toolchain (`ocx.toml` `zot = ...:2.1.21`).
- `registry:2` mirror: docker compose service `mirror-registry`, host port `${OCX_TEST_MIRROR_PORT:-5001}` -> 5000 (`test/docker-compose.yml:35-37`; also the legacy no-Referrers fixture). `CONTRIBUTING.md:56` "binds `localhost:5001`".
- toxiproxy bench: 5002, 8474 (profile `bench`, `docker-compose.yml:43-50`). Sigstore stack: compose profile `sigstore` (same file, from line ~56).
- ocx-mirror: dedicated zot per session; docs mention `localhost:5001` registry:2 compose and `:5002/:5011` ports in oracle runs (`.agents/goal/bazel-crate-split.md:59`).

**Can a config entry point at a local dir or localhost registry so another repo's `ocx.toml` resolves a local package?**
- `ocx.toml` has no `[registries]` table; `project-file.md` sections are `activate`, `env`, `group`, `lazy-mode`, `package`, `pinned`, `tools`, `trust`. Registry/index routing is in the config tiers (`config.toml`) and env.
- `[registries."<prefix>"] index = "file:///absolute/path"` points a namespace at a local index directory read with no server; relative paths and remote authority are refused (`website/.../reference/configuration.md:250-262`). The key is an identifier prefix (the package name's domain), not the host serving bytes (`:239-248`). Field presence is the "index-protocol" marker; omitted/`""` means plain OCI.
- A package named directly by registry host works without any index: `localhost:5001/<repo>:<tag>` in `[tools]` (inferred from the identifier grammar; the doc example `localhost:5000` appears in `guides/patch-packages.mdx:172`). Plain HTTP needs `OCX_INSECURE_REGISTRIES=localhost:5001` or `[registries."localhost:5001"] insecure = true` (`reference/environment.md:274-285`, `configuration.md:266-275`; `publishing/build-and-push.mdx:45`).
- `[mirrors."<host>"]` / `OCX_MIRRORS` replaces read traffic of a canonical host with another endpoint (`configuration.md:110-142`; `guides/corporate-mirror.mdx:120`: an `http://` endpoint needs the host to be plaintext-eligible). Identifier and digests are unchanged, so an `ocx.lock` stays valid (`configuration.md:121-123`). This is the documented route to serve `dev.ocx.sh` or `ocx.sh` names from a local registry (inferred applicability to a dev loop).
- Whether the lock records the registry host (it does: `ocx.lock` `repository = "dev.ocx.sh/ocx/cli"` in e2e-mirror) so a local identifier gets a local-host lock entry: yes by that evidence.

**Existing documentation of a local publish/consume loop.**
- `ocx-mirror:.claude/skills/e2e-test/SKILL.md:8-40` Tier 1 ("current `main` binary vs real specs, pushes to `localhost:5001`", `docker compose -f test/docker-compose.yml up -d`, `export OCX_INSECURE_REGISTRIES=localhost:5001`) and `references/local-pipeline.md` (not read in full). This is for ocx-mirror outputs, not an `ocx` binary.
- `ocx:test/manual/announce-e2e/README.md:20,140,331` and `scripts/clean_install_check.sh:11`: run with `OCX_BIN` set at a dev-channel-equivalent build (`target/release/ocx`) for `package announce`; not a package loop.
- No document found describing "publish my local ocx build as a package and have another repo resolve it".

## 6. Build information stamping

- **Mechanism (cargo)**: `crates/ocx_cli/build.rs` using `vergen_gix`. Emits git SHA (long), `describe` (tags + dirty marker), tracked-only dirty flag, commit timestamp, cargo target triple/debug, rustc semver (`build.rs:46-66`). `VERGEN_BUILD_TIMESTAMP` only when env `CI` is set (`:44-46`). Without `.git/` the git vars stay unset (`:56-57`). Pass-through env for the binary: `__OCX_BUILD_VERSION`, `__OCX_BUILD_CHANNEL` (`:70-72`) and `GITHUB_SERVER_URL|REPOSITORY|RUN_ID|WORKFLOW|REF|SHA` (`:74-84`).
- **Consumption**: `crates/ocx_cli/src/app/build_info.rs:99-147` (`option_env!`); `app/version.rs:9` effective version = `__OCX_BUILD_VERSION` else `CARGO_PKG_VERSION`. There is no branch variable; the nearest is `ci.git_ref` from `GITHUB_REF` when built under Actions (`build_info.rs:146`).
- **Printing**: `ocx version` (`--format json` for machine form; `ocx version --verbose` for the human provenance: channel, platform, commit short + clean/dirty + commit timestamp, build timestamp/target/rustc/profile, CI run URL; `api/data/version.rs:11-28,95-165`; docs `website/.../reference/command-line/commands.md:470-479`, `guides/update-ocx.mdx:129`). JSON also carries `contract` versions (`version.rs:26,43-50`).
- **Unstamped/test builds**: feature `__testing` makes `build.rs` bake `TESTING_PLACEHOLDERS` (SHA `0000...`, describe `placeholder-g00000000`, dirty `true`, timestamps `1970-01-01...`, `GITHUB_*` placeholders, channel `test`; `build.rs:19-35,92-106`). Bazel mirrors the table in `crates/ocx_cli/BUILD.bazel:32-60` (`_TESTING_PROVENANCE`, `crate_features = ["__testing"]` at `:85`, `stamp = 0` at `:103-115`); Bazel never runs `build.rs` and no `--stamp` / workspace-status command appears in `.bazelrc` (grep empty). BUILD.bazel states the graph is test-only: "releases stay on cargo-dist" (`:37`).
- **Release/dev guard**: `oci-publish.yml:173` runs `scripts/release_provenance_check.py --exec /usr/local/bin/ocx` before any push, and `scan-binaries.yml` byte-scans all targets, refusing a binary with test provenance.
- **`task cross:build TARGET=<triple>`** (`taskfiles/cross.taskfile.yml:12-21,40-55`): Bazel `-c opt --platforms=@rules_rs//rs/platforms:<triple> //crates/ocx_cli:ocx`, copied to `target/cross/<triple>/ocx[.exe]`. Supported triples (8): aarch64-apple-darwin, aarch64-pc-windows-gnullvm, aarch64-unknown-linux-gnu, aarch64-unknown-linux-musl, x86_64-apple-darwin, x86_64-pc-windows-gnullvm, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl. Consequence (inferred from the two facts above): such a binary carries the `__testing` placeholders (channel `test`, SHA zeros), would be refused by the provenance check, and uses windows-gnullvm, whereas release/dev packages are windows-msvc.

## 7. What cargo-dist produces for an ocx release

**`dist-workspace.toml`**: `cargo-dist-version = "0.31.0"`, `ci = "github"`, `hosting = "github"`, **`installers = []`** (no dist-generated installers, no dist updater), `source-tarball = false`, `cargo-auditable = true`, `cargo-cyclonedx = true`, `pr-run-mode = "skip"`.
- Targets (8): aarch64/x86_64 apple-darwin; aarch64/x86_64 unknown-linux-gnu; aarch64/x86_64 unknown-linux-musl; aarch64/x86_64 pc-windows-msvc. Custom runners: aarch64-pc-windows-msvc on `windows-11-arm` (native), aarch64-apple-darwin on `macos-15`.
- Archives: unix `.tar.gz` ("extraction must work on hosts without xz-utils"), windows `.zip`; names `ocx-<target>.tar.gz` / `ocx-<target>.zip` (consumers: `post-release-oci-publish` via `oci-publish.yml` "Extract cargo-dist archives", `distrib/ocx-${target}.${archive_ext}`; setup-ocx `getArchiveCandidates`). Archives include LICENSE* (including LICENSE-THIRD-PARTY.md) automatically (comment in dist-workspace.toml).
- Checksums: install docs say each release includes `sha256.sum` (`website/.../getting-started/install-ocx.mdx:218-226`); setup-ocx additionally fetches `<archive>.sha256` per archive (`src/download.ts:127`), so per-archive `.sha256` files exist too (inferred: both from dist's checksum step; not verified against a real release listing). SBOM: `*.cdx.xml` from `cargo cyclonedx` uploaded with the global artifacts (`release.yml` job `build-global-artifacts`, steps `cargo-cyclonedx`, ~`:214-258`). A `dist-manifest.json` per job, merged and removed before the release upload (`release.yml` "Cleanup").
- Release trigger: tag `**[0-9]+.[0-9]+.[0-9]+*` (`release.yml:42-44`); pre-release suffix => GitHub prerelease.
- GitHub release: `gh release create <tag> --target <sha> [--prerelease] --title <announcement_title> --notes-file <announcement_github_body> artifacts/*` (`release.yml` host job ~`:330-340`). Title and body come from dist's manifest (derived from the repo changelog; CHANGELOG.md is generated by git-cliff, so the body is that text, inferred).
- Extra jobs: custom local jobs `verify-version` and `readiness-gate` (a green `release-readiness.yml` run on the tag commit); global job `scan-binaries`; post-announce jobs `post-release-oci-publish`, `docker-publish`, `sdkgen-publish` (`dist-workspace.toml`).
  - `post-release-oci-publish.yml`: skipped for tags containing `-` (`:25,42`); tag `${GITHUB_REF_NAME#v}_${ts}`; pushes to `ghcr.io/ocx-sh/ocx/cli` (logical `ocx.sh/ocx/cli`, `physical_namespace: ocx-sh`), 6 musl+darwin+msvc targets (no gnu), `announce: true`, `artifact_source: cargo-dist`, env `ocx.sh` (`:46-65`). Secrets: workflow token for GHCR, `OCX_ANNOUNCE_TOKEN`. No signing step for `ocx/cli`: the only `ocx package sign` in ocx workflows is `sdkgen-publish.yml:257`.
  - `sdkgen-publish.yml`: packages `ocx-sdkgen` as `ghcr.io/ocx-sh/ocx/sdkgen`, signs keyless under that workflow's identity (only the `sign` job holds `id-token: write`), trust policy `packaging/sdkgen/trust-policy.toml`; not announced (header lines 1-22).
  - `docker-publish.yml`: `ghcr.io/ocx-sh/ocx` images (trixie + alpine, amd64/arm64) from the release archives via `gh release download`; also weekly cron and manual dispatch (`:1-30`).
- No `install.sh`/`install.ps1` in the ocx repo (only installer-authoring skill templates).

**Who downloads what.**
- `setup.ocx.sh` is served by nginx from `/data/www/setup` (`server-hetzner1:nginx/config/main/conf.d/ocx-sh-10-setup.conf`): `/sh|pwsh|nu|fish|elvish` => `latest/install.*`, `/<shell>/next` => `next/`, `/<shell>/<ver>` => `archive/<ver>/`, `/dist` => `dist.json`. Source of those scripts: separate repo `www-setup` (`src/install.sh|ps1|nu|fish|elv`).
- `install.sh` (`www-setup:src/install.sh:53,83-97,559-567,887-962`): reads manifest `OCX_INSTALL_DIST_URL` (default `https://setup.ocx.sh/dist.json`; flat leaf rows `{version, channel, tag, target, filename, sha256, url}`), picks the row for the target, downloads `url` (or `${OCX_INSTALL_MIRROR_URL}/<tag>/<filename>` when the mirror variable is set), verifies the inline sha256 (no `sha256.sum` fetch), extracts, finds the binary as `ocx-<target>/ocx` or flat `ocx`, smoke-tests `ocx version`, then runs `ocx self setup`. `OCX_INSTALL_REPO` default `ocx-sh/ocx` (error message points at its GitHub releases). `dist.json` is rebuilt from ocx's GitHub releases; the ocx side of that trigger is only a reference snippet (`www-setup:deploy/github/ocx-release-dispatch.yml.example`, repository_dispatch `ocx-released`, hourly cron fallback). install.ps1 not read.
- `ocx self update`: pulls from the OCI registry, not GitHub; filters out pre-releases and build-tagged versions (`website/.../reference/command-line/self.md:150-158`). So a dev tag is never picked up by self-update.
- setup-ocx: see Q3 (GitHub release archive `ocx-<target>.tar.gz|.tar.xz|.zip` plus `.sha256`).

**ocx-mirror release, for comparison** (`.github/workflows/release.yml`): not cargo-dist ("Lean by design ... no cargo-dist ceremony", `:1-8`). Tag `v[0-9]+.[0-9]+.[0-9]+*`; `verify` + `readiness` gates; `verify-version` (tag == `ocx_mirror` Cargo version); `build-matrix.yml` (Bazel-built legs, shared with Deploy Dev); `publish-registry` (skipped for `-` tags) to `ghcr.io/ocx-sh/ocx/mirror` (logical `ocx.sh/ocx/mirror`, `announce: true`); `github-release` job packages `ocx-mirror-<t>.tar.xz` (musl linux, darwin) and `ocx-mirror-<t>.zip` (windows gnullvm) with a `bin/ocx-mirror` layout, `sha256.sum`, git-cliff notes, `gh release create` (adds `--prerelease` if tag has `-`). Its version is stamped via `build-matrix.yml` inputs `version_semver` and `channel` (`release`/`dev`), no `build.rs` branch read here.

---

negative:
- No retention, prune or cleanup of `dev.ocx.sh` builds found in ocx, ocx-mirror or server-hetzner1 docs; no document describing the Artifactory repo's policy.
- No repo-visible description of who may push to `sh-ocx-oci-dev` beyond the secret names `REGISTRY_USER`/`REGISTRY_TOKEN` and the environment `dev.ocx.sh`.
- setup-ocx has no `registry`, `index` or dev-channel input; no way to select a `dev.ocx.sh` ocx through its `version:`.
- No task in ocx's `taskfile.yml`/`taskfiles/` packages or pushes `ocx/cli`; no doc of a "publish a local ocx build and consume it from another repo's `ocx.toml`" loop.
- `ocx.toml` has no `[registries]` table (registry/index routing is config-tier and env only).
- No branch stamp in the binary (only `GITHUB_REF` under Actions); Bazel builds carry no `--stamp` or workspace-status data.
- `ephemeral` appears nowhere in setup-ocx, e2e-mirror, ocx-mirror source, and the dev workflows do not use it.
- ocx's `ocx/cli` package is not signed by any workflow; ocx-mirror's `oci-publish.yml` was not checked for signing steps (only the setup/announce paths were read).
- index has no `ocx.toml`; e2e-package and e2e-index have no `ocx.toml`/`ocx.lock`.

leads:
- `ocx-mirror:.claude/skills/e2e-test/references/local-pipeline.md` (not read in full): local-registry recipes that may cover consuming a locally pushed package.
- `ocx:.github/workflows/build-matrix.yml`-equivalent in ocx-mirror and `scan-binaries.yml` / `scripts/release_provenance_check.py`: define exactly what a dev or local binary must report to be publishable.
- `ocx-mirror dist sync` (`crates/ocx_mirror_pipeline/src/dist_sync`): maintains the `dist/<sha256>.json` manifests the installers pin; the route by which a dev ocx build could be offered through `OCX_INSTALL_DIST_URL` / `OCX_INSTALL_MIRROR_URL` without a GitHub release.
- `www-setup:src/install.ps1`, `deploy/bunny/edge-rules.py`, `taskfiles/release.taskfile.yml` for the installer's channel pointers (`latest`, `next`, `archive/<ver>`).
- `server-hetzner1/registry/` (registry:3 + UI): confirm live or legacy; and the Artifactory UI for `sh-ocx-oci-dev` retention.
- `ocx:packaging/ocx/metadata.json` and `packaging/ocx/CATALOG.md`: what the pushed package declares, relevant to adding signing metadata.
- `ocx:.github/workflows/sdkgen-publish.yml:~200-260` and `packaging/sdkgen/trust-policy.toml`: the working in-repo template of keyless-sign-then-trust-policy for a first-party package.
- e2e-package `scripts/publish.sh`/`build.sh` (read only the workflow): the existing keyless-signed fixture publisher on ghcr.io.
