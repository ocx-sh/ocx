# Research: ocx bundle + rules_ocx current state

## Metadata

**Date:** 2026-11-17
**Lane:** codebase recon (`/hex-discuss` entry wave, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** ocx worktree `uno` at 5f1f5962; rules_ocx shallow clone at e311b99 (0.5.1)
**Expires:** 2027-05-17
**Caveat:** findings only, no recommendation; items marked UNVERIFIED had no primary source read.

### Findings

**0. Phase 1/2 building blocks in ocx.** All absent. I grepped `crates/` for `LayoutTransport|adopt_pins|fetch.plan|blob_url|PackageExport|PackageImport|bundle_path|bundle_files` and got zero hits. The ADR's own claim agrees: "ocx has no OCI image-layout reader or writer, and `ocx package copy` writes only to registries" (`research_package_bundle_recon.md:12`). The ADR is Proposed (`adr_package_export_import.md:5`) and still revised only on 2026-11-08. A `fetch_plan` or `bundle_files` report therefore has no consumer and no producer yet.

**1. rules_ocx modes** (clone is rules_ocx 0.5.1; ocx's own `MODULE.bazel:25` pins 0.5.0).
- **Shared `$OCX_HOME` eager is the default.** `make_ocx_env` reads `OCX_HOME` or `$HOME/.ocx` (`repo_utils.bzl:676-684`), and `AGENTS.md:39` says "Shared OCX_HOME by default".
- **`isolated_home`** puts the home at `ctx.path(".ocx_home")` (`repo_utils.bzl:676-677`).
- **`bins` (lazy)** writes launchers only, with nothing installed at fetch time (`package.bzl:52-102`). It requires a digest pin and is incompatible with `isolated_home` and `index` (`package.bzl:61-70`).
- **What the eager rule does.** It runs ocx via `rctx.execute`: `package install`, `package which`, `package env`, `package inspect --closure` (`package.bzl:187-282`).
  - It makes no `rctx.download` of package content.
  - It writes `ctx.symlink(root + "/content", "content")`, an absolute symlink into `$OCX_HOME/packages/.../content`, plus `entrypoints/` (`package.bzl:255-257`).
  - The generated files are real files: `launchers/*` (`ctx.file`, `package.bzl:283`), `env.bzl` and `BUILD.bazel`.
  - The `//:content` filegroup globs those symlinks (`package.bzl:290-295`).

**2. Platform selection.**
- **The host drives it.** `host_info(ctx.os.name, ctx.os.arch)` is called at `package.bzl:157`, `project.bzl:159` and `download.bzl:33`. Linux always maps to the musl ocx build (`platforms.bzl:97-103`).
- **A foreign platform is possible, but only for data.** `ocx.package(platforms=[…])` creates one repo per platform plus a hub (`extensions.bzl:287-313`).
  - Each repo runs on the host with `ocx package install -p <real>` (`package.bzl:179`).
  - The hub `select()`s on `config_setting(constraint_values)` of the **target** platform (`package.bzl:370-406`).
  - `examples/cross_platform` fetches jq for linux/arm64 and windows/amd64 through a platform transition on `//:content`.
- **Launchers exist only for the host platform.** `runnable = os_arch(real) == host.ocx_platform` (`package.bzl:185`, `project.bzl:240`). Foreign repos expose content only.
- **No exec-platform machinery.** There is no `toolchain()`, `exec_compatible_with` or `target_compatible_with` in `ocx/`.
  - The one mention is `platforms.bzl:48`, a docstring: "for toolchain authors composing exec_compatible_with".
  - `ocx_platform_constraints()` is the helper behind that docstring.
- **Cross-platform limits.** The libc/feature part of the platform is dropped for the constraints (`platforms.bzl:41-42`). The project tier rejects `platform` combined with `bins` (`project.bzl:193-196`).

**3. Launchers** (line numbers are from this clone).
- **Eager POSIX launcher** (`render_launcher`, `repo_utils.bzl:1585-1606`):
  - It exports `OCX_HOME` and `OCX_BINARY_PIN` (an absolute path to ocx under the output base), plus the constants.
  - It exports `PATH` as absolute `$OCX_HOME/packages/.../content` entries.
  - It then runs `exec '<abs target>'`.
  - It does not re-enter ocx directly, but the baked `OCX_BINARY_PIN` is for ocx entrypoint launchers inside the package (`repo_utils.bzl:1493-1494`).
- **Eager Windows launcher** is `.bat` with `%*` (`repo_utils.bzl:1582`).
- **Measured** (`research_package_bundle_launcher_bytes.md:29-42`): two homes give launchers that differ on the `OCX_HOME`, `PATH` and `exec` lines. The action-key effect was not measured (`:46-47`).
- **Lazy launcher** (`render_lazy_launcher`, `repo_utils.bzl:1325-1388`):
  - It re-enters `"$(rlocation <ocx runfile>)" package exec <pkg> -- <name>` at action time, and the ocx binary is a `data` runfile (`package.bzl:88,95`).
  - The script text is runfiles-relative and machine-independent on POSIX.
  - Windows lazy launchers bake absolute paths (`repo_utils.bzl:1356-1357, 1368`).
  - Content never becomes an action input.
  - The launcher's own comment says a manifest-only (sandboxed or remote) runfiles miss answers with exit 0 and empty output (`repo_utils.bzl:1310-1315`).
- **Implication.** An eager launcher needs `$OCX_HOME` populated on whichever machine runs the action. A lazy launcher needs that machine to run ocx there and have registry access plus a writable `OCX_HOME`.

**4. The `ocx` binary.** It comes from `ocx_download` as `@ocx_tool` (`download.bzl:28-59`).
- **One host-selected binary.** It downloads one release for the host (or the explicit `triple` attr, `download.bzl:34`), sha256-enforced from a vendored `dist/dist.json`.
- **The label.** `@ocx_tool//:ocx` is a build-graph label: `exports_files` plus a `filegroup` (`download.bzl:17-26`).
- **Where it actually sits in the graph.** The label is attached as `data` only to lazy launchers (`package.bzl:95`). Eager rules call it only inside repository rules.
- **No other platforms.** There is no per-platform or exec-configured ocx target.

**5. Remote execution, remote cache, vendor and related mentions.**
- **Remote execution is stated as a non-goal.** `docs/concepts/how-it-works.md:26`, `README.md:270` and `ocx/MODULE.bazel:135-137` say so, and the last one adds "There is no remote execution on this path, ever."
- **Remote cache is supported via `bins`.** `docs/guides/remote-cache.md` and `README.md:276-294` say keys are identical across machines on POSIX. A cold-machine race on the store is [ocx-sh/ocx#179](https://github.com/ocx-sh/ocx/issues/179).
- **Remote executor appears in design comments.**
  - `adr_0001_policy-tag-and-env-classes.md:155,184` covers the explicit env classes on remote executors.
  - `launcher_test.bzl:370-371`: a remote executor has no `OCX_HOME`, so a trusted root travels as a runfile.
  - Lazy staged config files are runfiles (`repo_utils.bzl:1241-1295`).
- **Downloader and vendor.**
  - `ocx_download` uses `download_and_extract` with sha256 (`download.bzl:45`).
  - `--credential_helper` appears only in `.agents/research/research_bazel-policy-surfaces.md:34`, as a future route.
  - No mention of `bazel vendor`, `--downloader_config`, `--repository_cache` handling or Build without the Bytes in `ocx/`, `docs/`, `e2e/` or `README.md`.
- **Reproducibility.** `extensions.bzl:331` returns `reproducible = True`. Floating tags resolve at fetch time and only print the digest (`package.bzl:198-205`).
- **ocx's own build** (`/home/mherwig/.herdr/worktrees/ocx/uno`):
  - `MODULE.bazel:130-137` says tool content is not an action input, a launcher execs an absolute store path, and `--remote_cache` is "the ceiling".
  - `.bazelrc` sets `--disk_cache` and a read-only remote cache.
  - `test/bazel.bzl:260` carries `@tools//:zot` as `data`.

**6. Installed package shape.** Cited from `subsystem-file-structure.md`; file counts are stated nowhere I found.
- **Layout.** Packages are keyed by registry plus digest (`:15`). Each dir has `content/`, `metadata.json`, `manifest.json`, `resolve.json`, `install.json`, `digest` and `refs/{symlinks,deps,layers,blobs,origins,verified}` (`:138`).
- **Hardlinks.** `content/` is hardlinked from `layers/` (`:15`). `$OCX_HOME` must sit on one volume (`:392`).
- **Dependencies.** `refs/deps/<alg>_<32hex>` are symlinks to the dependency's `content/` (`:338`). rules_ocx exposes only the root's `content/` and `entrypoints/`.
- **Entrypoints** are generated launchers, and `package which` returns `{path, kind}` (`AGENTS.md:135-143`).
- **macOS codesign** happens after extraction (system design `:11`). Not measured under Bazel.

**7. The ADR's "materialiser C".**
- **What it requires of ocx** (nothing exists yet):
  - `package export` and `package import`, with `LayoutTransport`, `ContainedDir`, `adopt_pins`, referrer markers and a local-first referrer read.
  - `inspect --fetch-plan` and `Client::blob_url`.
  - `--offline package import --no-verify` into a repository-local `OCX_HOME`.
  - `package which` working for a never-installed package (UNVERIFIED, SD §14.5).
- **What it requires of rules_ocx** (ADR Decision 6, system design `:118-139`):
  - New spoke repository rules: plan, `rctx.download(sha256, canonical_id)`, `rctx.file(bundle_files)`, import, `which`.
  - Launchers rendered from plan segments, runfiles-relative.
  - `isolated_home` deleted, `bins` deprecated, a native Windows launcher.
  - Credential-helper docs (exact hosts) and a pinned minimum Bazel.
- **Measured vs UNVERIFIED.**
  - Measured: launcher bytes differ across homes (as in item 3).
  - UNVERIFIED:
    - which Bazel release carries commit 4f13ff7 (ADR `:68`);
    - `bazel vendor` on `refs/` symlinks (`:119`);
    - whether `block` applies to redirect targets (SD `:611`);
    - ten items in SD §14, including Windows `ContainedDir` and whether cosign bundles embed the Rekor proof.
- **Spikes.** S1, S2b and S2a are not run (no results artifacts). S3 is "runs inside WP-7", which has not been built.
- **Sentences on remote execution or remote cache.** None in the ADR beyond two items.
  - Driver 6 and the Decision 6 narrative cover "Hermetic Bazel" and "hermetic action keys". They include "cache/vendor/`--downloader_config`" (`:51,97`).
  - Out of scope: "Remote execution for ocx's own build" (`:405`).
  - The system design has only repository-cache and `--downloader_config` rows (`:74, 613, 622, 624, 693-698`). "Action keys identical across two `OCX_HOME`s" (`:698`) is the nearest. No row tests execution on a remote executor, nor a Build-without-the-Bytes build.
  - `ocx` is absent from step 6's launcher design; nothing says how the executor gets ocx or the repository-local home.

**8. Frozen for Bazel if export/import ships first.** The ADR names the bundle layout `blobs/<alg>/<hex>`, `oci-layout` and `index.json` (B-1 to B-11, SD §5.2) as a persisted contract. `bundle_path` and `bundle_files` appear in the plan (SD §5.6), not in the Phase 1 report; they are frozen when Phase 2 ships and rules_ocx consumes them. The ADR header calls both "one-way doors" (`:10-11`). The plan-to-import test is the stated guard (`SD:691`).

### negative:
- No building block exists in `crates/`; no spike result artifact exists.
- No toolchain, `exec_compatible_with` or per-platform ocx in rules_ocx.
- No `rctx.download` of package content, `bazel vendor`, `--downloader_config` or Build-without-the-Bytes handling in rules_ocx.
- No file-count figures for installed packages.
- **Line-citation drift.** The ADR cites `repo_utils.bzl:1516,1536,1540`; the launcher bake and `.bat %*` are at 1562/1586 and 1582 in this clone (the launcher-bytes research cites 1519-1520). `:51` and `:72-76` still match. These are all against rules_ocx 0.5.1 vs 0.5.0.
- The ADR says "Launchers unset any inherited `OCX_BINARY_PIN`"; current eager launchers set it.

### leads:
- Bazel docs/source: how `rctx.execute` and the repo-contents cache behave when a repository rule's output holds absolute symlinks or hardlinks (feeds S2b).
- Measure the action-key effect with `--execution_log_compact_file` across two homes (open in the launcher-bytes research).
- A third lane on RBE mechanics: what runs ocx on the executor, and how a remote workspace gets content (not in this lane's files).
- rules_ocx issue tracker and [ocx-sh/ocx#179](https://github.com/ocx-sh/ocx/issues/179) for the lazy-mode store race.

Skipped: `research_bazel_downloader_oci_blobs.md` read via grep only; rules_ocx `e2e/` and `docs/` not read in full; no file counts measured.
